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
    use crate::cursor::{self, CursorShape, CursorState};
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
        /// Latest desktop image without the pointer, tightly packed BGRA.
        desktop: Vec<u8>,
        shape: CursorShape,
        cursor: CursorState,
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
                    desktop: Vec::new(),
                    shape: CursorShape::default(),
                    cursor: CursorState::default(),
                })
            }
        }

        /// Wait up to `timeout_ms` for a desktop or pointer change, and return the desktop
        /// with the pointer drawn on it.
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
                let result = self.absorb(&info, resource);
                self.duplication.ReleaseFrame()?;
                let changed = result?;
                if !changed || self.desktop.is_empty() {
                    return Ok(Captured::Idle);
                }
                let mut bgra = self.desktop.clone();
                cursor::composite(
                    &mut bgra,
                    self.width as usize,
                    self.height as usize,
                    &self.shape,
                    self.cursor,
                );
                Ok(Captured::Frame(Frame { width: self.width, height: self.height, bgra }))
            }
        }

        /// Update desktop/pointer state from an acquired frame. Returns true if anything changed.
        unsafe fn absorb(&mut self, info: &DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>) -> Result<bool> {
            let mut changed = false;

            if info.LastPresentTime != 0 || self.desktop.is_empty() {
                let resource = resource.context("no frame resource")?;
                let tex: ID3D11Texture2D = resource.cast()?;
                self.context.CopyResource(&self.staging, &tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                let pitch = mapped.RowPitch as usize;
                let src = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * self.height as usize);
                self.desktop = unpitch(src, pitch, self.width as usize * 4, self.height as usize);
                self.context.Unmap(&self.staging, 0);
                changed = true;
            }

            if info.LastMouseUpdateTime != 0 {
                let p = info.PointerPosition;
                self.cursor = CursorState { visible: p.Visible.as_bool(), x: p.Position.x, y: p.Position.y };
                changed = true;
            }

            if info.PointerShapeBufferSize > 0 {
                let mut buf = vec![0u8; info.PointerShapeBufferSize as usize];
                let mut needed = 0u32;
                let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                self.duplication.GetFramePointerShape(
                    buf.len() as u32,
                    buf.as_mut_ptr() as *mut _,
                    &mut needed,
                    &mut si,
                )?;
                let height = if si.Type == cursor::SHAPE_MONOCHROME { si.Height / 2 } else { si.Height };
                self.shape = CursorShape { kind: si.Type, width: si.Width, height, pitch: si.Pitch, data: buf };
                changed = true;
            }
            Ok(changed)
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
