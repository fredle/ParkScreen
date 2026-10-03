//! Screen capture of a single monitor using DXGI Desktop Duplication.

use anyhow::Result;

/// One captured frame: tightly packed BGRA8, `width * 4` bytes per row.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub enum Captured {
    Frame(Frame),
    /// Nothing changed within the timeout.
    Idle,
    /// The duplication was invalidated (mode change, UAC, lock screen...). Recreate it.
    Lost,
}

/// Copy `rows` rows of `row_bytes` each from a source with `pitch` bytes per row into a
/// tightly packed buffer.
pub fn unpitch(src: &[u8], pitch: usize, row_bytes: usize, rows: usize) -> Vec<u8> {
    if pitch == row_bytes {
        return src[..row_bytes * rows].to_vec();
    }
    let mut out = Vec::with_capacity(row_bytes * rows);
    for r in 0..rows {
        out.extend_from_slice(&src[r * pitch..r * pitch + row_bytes]);
    }
    out
}

#[cfg(windows)]
pub use win::Duplicator;

#[cfg(not(windows))]
pub struct Duplicator;

#[cfg(not(windows))]
impl Duplicator {
    pub fn new(_device_name: &str) -> Result<Self> {
        anyhow::bail!("capture is only supported on Windows")
    }
    pub fn next_frame(&mut self, _timeout_ms: u32) -> Result<Captured> {
        unreachable!()
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use anyhow::{anyhow, Context};
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Dxgi::*;

    pub struct Duplicator {
        context: ID3D11DeviceContext,
        duplication: IDXGIOutputDuplication,
        staging: ID3D11Texture2D,
        width: u32,
        height: u32,
        /// Last frame returned, for the case where only the pointer moved.
        has_frame: bool,
    }

    fn wide_eq(w: &[u16], s: &str) -> bool {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end]).eq_ignore_ascii_case(s)
    }

    impl Duplicator {
        /// Start duplicating the output whose GDI name is `device_name` (e.g. `\\.\DISPLAY3`).
        pub fn new(device_name: &str) -> Result<Self> {
            unsafe {
                let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
                let mut found: Option<(IDXGIAdapter1, IDXGIOutput)> = None;
                let mut ai = 0;
                'outer: while let Ok(adapter) = factory.EnumAdapters1(ai) {
                    ai += 1;
                    let mut oi = 0;
                    while let Ok(output) = adapter.EnumOutputs(oi) {
                        oi += 1;
                        let desc = output.GetDesc()?;
                        if wide_eq(&desc.DeviceName, device_name) {
                            found = Some((adapter.clone(), output));
                            break 'outer;
                        }
                    }
                }
                let (adapter, output) =
                    found.ok_or_else(|| anyhow!("DXGI has no output named {device_name}"))?;

                let mut device: Option<ID3D11Device> = None;
                let mut context: Option<ID3D11DeviceContext> = None;
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
                .context("creating D3D11 device")?;
                let device = device.context("no D3D11 device")?;
                let context = context.context("no D3D11 context")?;

                let output1: IDXGIOutput1 = output.cast()?;
                let duplication = output1
                    .DuplicateOutput(&device)
                    .context("DuplicateOutput failed (another capture app, or no desktop session?)")?;

                let dd = duplication.GetDesc();
                let (width, height) = (dd.ModeDesc.Width, dd.ModeDesc.Height);

                let tex_desc = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut staging: Option<ID3D11Texture2D> = None;
                device.CreateTexture2D(&tex_desc, None, Some(&mut staging))?;

                Ok(Self {
                    context,
                    duplication,
                    staging: staging.context("no staging texture")?,
                    width,
                    height,
                    has_frame: false,
                })
            }
        }

        /// Wait up to `timeout_ms` for a new desktop image.
        pub fn next_frame(&mut self, timeout_ms: u32) -> Result<Captured> {
            unsafe {
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                match self.duplication.AcquireNextFrame(timeout_ms, &mut info, &mut resource) {
                    Ok(()) => {}
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Captured::Idle),
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Ok(Captured::Lost),
                    Err(e) => return Err(e.into()),
                }
                // Pointer-only updates carry no new desktop pixels. W1 draws no cursor,
                // so skip them (unless this is the very first frame).
                if info.LastPresentTime == 0 && self.has_frame {
                    self.duplication.ReleaseFrame()?;
                    return Ok(Captured::Idle);
                }
                let result = (|| -> Result<Frame> {
                    let resource = resource.context("no frame resource")?;
                    let tex: ID3D11Texture2D = resource.cast()?;
                    self.context.CopyResource(&self.staging, &tex);
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    self.context
                        .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                    let pitch = mapped.RowPitch as usize;
                    let len = pitch * self.height as usize;
                    let src = std::slice::from_raw_parts(mapped.pData as *const u8, len);
                    let bgra = unpitch(src, pitch, self.width as usize * 4, self.height as usize);
                    self.context.Unmap(&self.staging, 0);
                    Ok(Frame { width: self.width, height: self.height, bgra })
                })();
                self.duplication.ReleaseFrame()?;
                self.has_frame = true;
                Ok(Captured::Frame(result?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpitch_strips_row_padding() {
        // 2 rows of 3 bytes, pitch 5
        let src = [1, 2, 3, 0, 0, 4, 5, 6, 0, 0];
        assert_eq!(unpitch(&src, 5, 3, 2), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn unpitch_passthrough_when_tight() {
        let src = [1, 2, 3, 4, 5, 6];
        assert_eq!(unpitch(&src, 3, 3, 2), src.to_vec());
    }
}
