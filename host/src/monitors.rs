//! Monitor enumeration and display-mode changes.

use anyhow::{bail, Context, Result};
use std::fmt;
use std::str::FromStr;

/// A requested display mode such as `1920x1200@60`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorMode {
    pub width: u32,
    pub height: u32,
    pub hz: Option<u32>,
}

impl FromStr for MonitorMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim().to_ascii_lowercase();
        let (size, hz) = match s.split_once('@') {
            Some((size, hz)) => (size, Some(hz.trim_end_matches("hz").parse::<u32>()?)),
            None => (s.as_str(), None),
        };
        let (w, h) = size
            .split_once('x')
            .with_context(|| format!("mode '{s}' must look like 1920x1200 or 1920x1200@60"))?;
        let mode = MonitorMode { width: w.parse()?, height: h.parse()?, hz };
        if mode.width == 0 || mode.height == 0 {
            bail!("mode '{s}' has a zero dimension");
        }
        Ok(mode)
    }
}

impl fmt::Display for MonitorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)?;
        if let Some(hz) = self.hz {
            write!(f, "@{hz}")?;
        }
        Ok(())
    }
}

/// Name fragments that suggest a virtual display driver.
const VIRTUAL_HINTS: &[&str] = &["parkscreen", "vdd", "virtual", "idd", "usbmmidd", "spacedesk"];

pub fn looks_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    VIRTUAL_HINTS.iter().any(|h| n.contains(h))
}

#[derive(Debug, Clone)]
pub struct Monitor {
    /// Position in the list shown by `list` (1-based).
    pub index: usize,
    /// GDI device name, e.g. `\\.\DISPLAY3`.
    pub device_name: String,
    /// Monitor friendly name from the display config API, if known.
    pub friendly_name: String,
    /// Adapter description, e.g. "Parsec Virtual Display Adapter".
    pub adapter: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub primary: bool,
}

impl Monitor {
    pub fn is_virtual(&self) -> bool {
        looks_virtual(&self.friendly_name) || looks_virtual(&self.adapter)
    }
}

/// How the user picked a monitor on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Index(usize),
    Name(String),
    Auto,
}

impl FromStr for Selector {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.parse::<usize>() {
            Ok(i) => Selector::Index(i),
            Err(_) if s.eq_ignore_ascii_case("auto") => Selector::Auto,
            Err(_) => Selector::Name(s.to_string()),
        })
    }
}

/// Choose a monitor from `monitors` according to `sel`.
pub fn select<'a>(monitors: &'a [Monitor], sel: &Selector) -> Result<&'a Monitor> {
    match sel {
        Selector::Index(i) => monitors
            .iter()
            .find(|m| m.index == *i)
            .with_context(|| format!("no monitor with index {i}")),
        Selector::Name(n) => {
            let n = n.to_ascii_lowercase();
            monitors
                .iter()
                .find(|m| {
                    m.device_name.to_ascii_lowercase() == n
                        || m.friendly_name.to_ascii_lowercase().contains(&n)
                        || m.adapter.to_ascii_lowercase().contains(&n)
                })
                .with_context(|| format!("no monitor matching '{n}'"))
        }
        Selector::Auto => monitors
            .iter()
            .find(|m| m.is_virtual())
            .or_else(|| monitors.iter().find(|m| !m.primary))
            .context(
                "no virtual or secondary monitor found; install a virtual display driver \
                 (e.g. Virtual Display Driver) or pass --monitor",
            ),
    }
}

#[cfg(windows)]
pub use win::{enumerate, set_mode};

#[cfg(not(windows))]
pub fn enumerate() -> Result<Vec<Monitor>> {
    bail!("monitor enumeration is only supported on Windows")
}

#[cfg(not(windows))]
pub fn set_mode(_device: &str, _mode: MonitorMode) -> Result<()> {
    bail!("display mode changes are only supported on Windows")
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::collections::HashMap;
    use windows::core::PCWSTR;
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Graphics::Gdi::*;

    fn wide_to_string(w: &[u16]) -> String {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Map GDI device name (`\\.\DISPLAYn`) to monitor friendly name.
    fn friendly_names() -> HashMap<String, String> {
        let mut out = HashMap::new();
        unsafe {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != windows::Win32::Foundation::WIN32_ERROR(0) {
                return out;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            if QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            ) != windows::Win32::Foundation::WIN32_ERROR(0)
            {
                return out;
            }
            for p in &paths[..np as usize] {
                let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                src.header.size = std::mem::size_of_val(&src) as u32;
                src.header.adapterId = p.sourceInfo.adapterId;
                src.header.id = p.sourceInfo.id;
                if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                    continue;
                }
                let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
                tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
                tgt.header.size = std::mem::size_of_val(&tgt) as u32;
                tgt.header.adapterId = p.targetInfo.adapterId;
                tgt.header.id = p.targetInfo.id;
                if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                    continue;
                }
                out.insert(
                    wide_to_string(&src.viewGdiDeviceName),
                    wide_to_string(&tgt.monitorFriendlyDeviceName),
                );
            }
        }
        out
    }

    pub fn enumerate() -> Result<Vec<Monitor>> {
        let names = friendly_names();
        let mut monitors = Vec::new();
        let mut i = 0u32;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
                break;
            }
            i += 1;
            if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP).0 == 0 {
                continue;
            }
            let device_name = wide_to_string(&dd.DeviceName);
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            let wname = to_wide(&device_name);
            if !unsafe { EnumDisplaySettingsW(PCWSTR(wname.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
                .as_bool()
            {
                continue;
            }
            let pos = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
            monitors.push(Monitor {
                index: monitors.len() + 1,
                friendly_name: names.get(&device_name).cloned().unwrap_or_default(),
                adapter: wide_to_string(&dd.DeviceString),
                device_name,
                x: pos.x,
                y: pos.y,
                width: dm.dmPelsWidth,
                height: dm.dmPelsHeight,
                hz: dm.dmDisplayFrequency,
                primary: (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE).0 != 0,
            });
        }
        Ok(monitors)
    }

    /// Switch `device` (e.g. `\\.\DISPLAY3`) to `mode`. The driver must offer the mode.
    pub fn set_mode(device: &str, mode: MonitorMode) -> Result<()> {
        let wname = to_wide(device);
        let mut dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !unsafe { EnumDisplaySettingsW(PCWSTR(wname.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
            .as_bool()
        {
            bail!("could not read current settings of {device}");
        }
        dm.dmPelsWidth = mode.width;
        dm.dmPelsHeight = mode.height;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if let Some(hz) = mode.hz {
            dm.dmDisplayFrequency = hz;
            dm.dmFields |= DM_DISPLAYFREQUENCY;
        }
        // Validate first so a bad mode gives a clear error instead of a half-applied change.
        let test = unsafe {
            ChangeDisplaySettingsExW(PCWSTR(wname.as_ptr()), Some(&dm), None, CDS_TEST, None)
        };
        if test != DISP_CHANGE_SUCCESSFUL {
            bail!("{device} does not support {mode} (test returned {})", test.0);
        }
        let r = unsafe {
            ChangeDisplaySettingsExW(PCWSTR(wname.as_ptr()), Some(&dm), None, CDS_UPDATEREGISTRY, None)
        };
        if r != DISP_CHANGE_SUCCESSFUL {
            bail!("failed to set {mode} on {device} (code {})", r.0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(index: usize, dev: &str, friendly: &str, adapter: &str, primary: bool) -> Monitor {
        Monitor {
            index,
            device_name: dev.into(),
            friendly_name: friendly.into(),
            adapter: adapter.into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            hz: 60,
            primary,
        }
    }

    #[test]
    fn parses_modes() {
        assert_eq!("1920x1200@60".parse::<MonitorMode>().unwrap(), MonitorMode { width: 1920, height: 1200, hz: Some(60) });
        assert_eq!("2200X1300".parse::<MonitorMode>().unwrap(), MonitorMode { width: 2200, height: 1300, hz: None });
        assert_eq!("1280x720@30hz".parse::<MonitorMode>().unwrap().hz, Some(30));
        assert!("1920".parse::<MonitorMode>().is_err());
        assert!("0x1080".parse::<MonitorMode>().is_err());
        assert!("axb".parse::<MonitorMode>().is_err());
    }

    #[test]
    fn mode_display_round_trips() {
        let m: MonitorMode = "1920x1200@60".parse().unwrap();
        assert_eq!(m.to_string(), "1920x1200@60");
    }

    #[test]
    fn auto_prefers_virtual_monitor() {
        let ms = vec![
            mon(1, r"\\.\DISPLAY1", "DELL U2720Q", "NVIDIA GeForce", true),
            mon(2, r"\\.\DISPLAY2", "LG TV", "NVIDIA GeForce", false),
            mon(3, r"\\.\DISPLAY3", "VDD by MTT", "Virtual Display Adapter", false),
        ];
        assert_eq!(select(&ms, &Selector::Auto).unwrap().index, 3);
    }

    #[test]
    fn auto_falls_back_to_non_primary() {
        let ms = vec![
            mon(1, r"\\.\DISPLAY1", "DELL", "NVIDIA", true),
            mon(2, r"\\.\DISPLAY2", "LG TV", "NVIDIA", false),
        ];
        assert_eq!(select(&ms, &Selector::Auto).unwrap().index, 2);
    }

    #[test]
    fn auto_fails_with_only_primary() {
        let ms = vec![mon(1, r"\\.\DISPLAY1", "DELL", "NVIDIA", true)];
        assert!(select(&ms, &Selector::Auto).is_err());
    }

    #[test]
    fn selects_by_index_and_name() {
        let ms = vec![
            mon(1, r"\\.\DISPLAY1", "DELL", "NVIDIA", true),
            mon(2, r"\\.\DISPLAY5", "ParkScreen", "IDD", false),
        ];
        assert_eq!("2".parse::<Selector>().unwrap(), Selector::Index(2));
        assert_eq!(select(&ms, &Selector::Index(2)).unwrap().device_name, r"\\.\DISPLAY5");
        assert_eq!(select(&ms, &"parkscreen".parse().unwrap()).unwrap().index, 2);
        assert_eq!(select(&ms, &r"\\.\display1".parse().unwrap()).unwrap().index, 1);
        assert!(select(&ms, &Selector::Index(9)).is_err());
    }
}
