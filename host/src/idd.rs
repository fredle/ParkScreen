//! Control of the ParkScreen virtual display driver (`driver/`): adds a monitor to the desktop
//! while a car is connected, so the car can be an extra screen instead of a copy.

use crate::display::Mode;

/// Device interface GUID of the driver, `GUID_DEVINTERFACE_PARKSCREEN` in `Ioctl.h`.
pub const INTERFACE_GUID: u128 = 0xa3c2e1b4_6f5d_4b7a_9e28_1c4d7f0b8a65;

/// Named pipe the driver serves, `PARKSCREEN_PIPE_NAME` in `Ioctl.h`. (Custom IOCTLs do not reach
/// the driver: it is a display adapter.) One request per message: a little-endian u32 function,
/// then the input; the reply is a little-endian i32 NTSTATUS, then the output.
pub const PIPE_NAME: &str = r"\\.\pipe\ParkScreenIdd";

pub const FUNC_PLUG: u32 = 0x800;
pub const FUNC_UNPLUG: u32 = 0x801;
#[allow(dead_code)]
pub const FUNC_STATUS: u32 = 0x802;

pub const MIN_SIZE: u32 = 320;
pub const MAX_SIZE: u32 = 4095;
pub const MIN_HZ: u32 = 24;
pub const MAX_HZ: u32 = 120;

/// Reject modes the driver cannot present.
pub fn validate(mode: Mode) -> Result<(), String> {
    let ok = (MIN_SIZE..=MAX_SIZE).contains(&mode.width)
        && (MIN_SIZE..=MAX_SIZE).contains(&mode.height)
        && (MIN_HZ..=MAX_HZ).contains(&mode.refresh_hz);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "the display driver cannot present {}x{}@{} ({MIN_SIZE}..{MAX_SIZE} pixels, {MIN_HZ}..{MAX_HZ} Hz)",
            mode.width, mode.height, mode.refresh_hz
        ))
    }
}

/// Input of `FUNC_PLUG` (`ParkScreenMode`): three little-endian u32.
pub fn plug_bytes(mode: Mode) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&mode.width.to_le_bytes());
    b[4..8].copy_from_slice(&mode.height.to_le_bytes());
    b[8..12].copy_from_slice(&mode.refresh_hz.to_le_bytes());
    b
}

#[cfg(windows)]
pub use win::{driver_installed, IddDisplay};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::{display::DisplayBackend, monitors};
    use async_trait::async_trait;
    use std::time::{Duration, Instant};
    use windows::{
        core::{GUID, PCWSTR},
        Win32::{
            Devices::{
                DeviceAndDriverInstallation::{
                    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
                    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
                    SP_DEVICE_INTERFACE_DETAIL_DATA_W,
                },
                Display::{SetDisplayConfig, SDC_APPLY, SDC_TOPOLOGY_EXTEND},
            },
        },
    };

    /// How long to wait for Windows to bring the new monitor up after a plug.
    const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(10);

    /// Path of the first present ParkScreen display device, if the driver is installed.
    fn device_path() -> Option<Vec<u16>> {
        let guid = GUID::from_u128(INTERFACE_GUID);
        unsafe {
            let set = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).ok()?;
            let path = (|| {
                let mut data = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
                SetupDiEnumDeviceInterfaces(set, None, &guid, 0, &mut data).ok()?;
                let mut needed = 0u32;
                let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
                if needed == 0 {
                    return None;
                }
                // 8-byte aligned storage for the variable-length detail structure.
                let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
                let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), needed, None, None).ok()?;
                let start = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
                let mut len = 0;
                while *start.add(len) != 0 {
                    len += 1;
                }
                let mut v = std::slice::from_raw_parts(start, len).to_vec();
                v.push(0);
                Some(v)
            })();
            let _ = SetupDiDestroyDeviceInfoList(set);
            path
        }
    }

    /// True when the ParkScreen display driver is installed and its device is started.
    pub fn driver_installed() -> bool {
        device_path().is_some()
    }

    /// Connection to the driver's control pipe. The monitor lives as long as the connection: if the
    /// agent dies the driver removes the monitor.
    struct Device(std::fs::File);

    impl Device {
        fn open() -> Result<Self, String> {
            if !driver_installed() {
                return Err("the ParkScreen display driver is not installed".into());
            }
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_NAME)
                .map_err(|e| format!("could not connect to the display driver: {e}"))?;
            Ok(Device(f))
        }

        /// One request/reply round trip; returns the driver's output.
        fn control(&mut self, function: u32, input: &[u8]) -> Result<Vec<u8>, String> {
            use std::io::{Read, Write};
            let mut msg = function.to_le_bytes().to_vec();
            msg.extend_from_slice(input);
            self.0.write_all(&msg).map_err(|e| format!("display driver request failed: {e}"))?;
            let mut reply = [0u8; 64];
            let n = self.0.read(&mut reply).map_err(|e| format!("display driver reply failed: {e}"))?;
            if n < 4 {
                return Err("display driver sent a short reply".into());
            }
            let status = i32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]]);
            if status < 0 {
                return Err(format!("display driver refused the request (status 0x{:08x})", status as u32));
            }
            Ok(reply[4..n].to_vec())
        }
    }

    /// `DisplayBackend` that adds a ParkScreen monitor to the desktop (extend mode).
    #[derive(Default)]
    pub struct IddDisplay {
        device: Option<Device>,
        current: Option<Mode>,
    }

    fn is_parkscreen(m: &monitors::Monitor) -> bool {
        m.friendly_name.to_ascii_lowercase().contains("parkscreen")
    }

    /// Block until the ParkScreen monitor is active at `mode`, and make sure it extends the
    /// desktop rather than copying another monitor.
    fn wait_for_monitor(mode: Mode) -> Result<(), String> {
        let start = Instant::now();
        loop {
            if let Ok(all) = monitors::enumerate() {
                if let Some(m) = all.iter().find(|m| is_parkscreen(m) && (m.width, m.height) == (mode.width, mode.height)) {
                    let primary = all.iter().find(|p| p.primary);
                    // Windows can remember "duplicate" from an earlier display setup.
                    if primary.is_some_and(|p| (p.x, p.y) == (m.x, m.y) && p.device_name != m.device_name) {
                        let r = unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) };
                        tracing::info!("virtual display was duplicating; asked Windows to extend (result {r})");
                    }
                    tracing::info!("virtual display ready: {} {}x{}", m.device_name, m.width, m.height);
                    return Ok(());
                }
            }
            if start.elapsed() > ARRIVAL_TIMEOUT {
                return Err("the ParkScreen display did not appear in time".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    impl IddDisplay {
        fn plug_blocking(&mut self, mode: Mode) -> Result<(), String> {
            validate(mode)?;
            if self.device.is_none() {
                self.device = Some(Device::open()?);
            }
            let dev = self.device.as_mut().expect("opened above");
            if let Err(e) = dev.control(FUNC_PLUG, &plug_bytes(mode)) {
                // A broken connection (driver restarted) is reopened on the next try.
                self.device = None;
                return Err(e);
            }
            self.current = Some(mode);
            wait_for_monitor(mode)
        }

        fn unplug_blocking(&mut self) -> Result<(), String> {
            self.current = None;
            match &mut self.device {
                Some(dev) => dev.control(FUNC_UNPLUG, &[]).map(|_| ()),
                None => Ok(()),
            }
        }
    }

    #[async_trait]
    impl DisplayBackend for IddDisplay {
        async fn plug(&mut self, mode: Mode) -> Result<(), String> {
            // The calls block (driver round trip, waiting for Windows), so keep them off the runtime.
            let mut me = std::mem::take(self);
            let (me, r) = tokio::task::spawn_blocking(move || {
                let r = me.plug_blocking(mode);
                (me, r)
            })
            .await
            .map_err(|e| e.to_string())?;
            *self = me;
            r
        }

        async fn unplug(&mut self) -> Result<(), String> {
            self.unplug_blocking()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = include_str!("../../driver/ParkScreenIdd/Ioctl.h");

    #[test]
    fn pipe_name_matches_the_driver_header() {
        assert!(HEADER.contains(r#"#define PARKSCREEN_PIPE_NAME L"\\\\.\\pipe\\ParkScreenIdd""#));
        assert_eq!(PIPE_NAME, r"\\.\pipe\ParkScreenIdd");
    }

    #[test]
    fn matches_the_driver_header() {
        for (name, value) in [("PLUG", "0x800"), ("UNPLUG", "0x801"), ("STATUS", "0x802")] {
            let line = HEADER.lines().find(|l| l.starts_with(&format!("#define PARKSCREEN_FUNC_{name} "))).unwrap();
            assert!(line.contains(value), "{line}");
        }
        for (name, value) in [("MIN_SIZE", MIN_SIZE), ("MAX_SIZE", MAX_SIZE), ("MIN_HZ", MIN_HZ), ("MAX_HZ", MAX_HZ)] {
            let line = HEADER.lines().find(|l| l.starts_with(&format!("#define PARKSCREEN_{name} "))).unwrap();
            assert!(line.ends_with(&value.to_string()), "{line}");
        }
        let g = INTERFACE_GUID;
        let expect = format!(
            "0x{:08x}, 0x{:04x}, 0x{:04x}, {}",
            (g >> 96) as u32,
            (g >> 80) as u16,
            (g >> 64) as u16,
            (0..8).map(|i| format!("0x{:02x}", (g >> (56 - 8 * i)) as u8)).collect::<Vec<_>>().join(", ")
        );
        assert!(HEADER.contains(&expect), "GUID mismatch: {expect}");
    }

    #[test]
    fn validates_modes() {
        let m = |w, h, hz| Mode { width: w, height: h, refresh_hz: hz };
        assert!(validate(m(1920, 1080, 60)).is_ok());
        assert!(validate(m(4095, 4095, 120)).is_ok());
        assert!(validate(m(7680, 4320, 60)).is_err());
        assert!(validate(m(1920, 1080, 10)).is_err());
        assert!(validate(m(100, 1080, 60)).is_err());
    }

    #[test]
    fn plug_payload_layout() {
        let b = plug_bytes(Mode { width: 1, height: 2, refresh_hz: 3 });
        assert_eq!(b, [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
    }
}
