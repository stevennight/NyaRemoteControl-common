//! DXGI Desktop Duplication wrapper. Must be created on the adapter that
//! drives the output (design doc §3.5) and on a thread attached to the
//! current input desktop (see [`crate::desktop`]).

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput, IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
    DXGI_ERROR_INVALID_CALL, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

use crate::d3d::D3dDevice;

#[derive(Debug, thiserror::Error)]
pub enum DupError {
    /// Desktop switch, mode change, fullscreen transition: recreate the duplicator.
    #[error("desktop duplication access lost")]
    AccessLost,
    /// Driver reset / GPU removed (e.g. MUX switch): rebuild devices and topology.
    #[error("GPU device removed or reset")]
    DeviceLost,
    #[error("{0}")]
    Other(#[from] windows::core::Error),
}

fn classify(e: windows::core::Error) -> DupError {
    let c = e.code();
    if c == DXGI_ERROR_ACCESS_LOST || c == DXGI_ERROR_INVALID_CALL {
        DupError::AccessLost
    } else if c == DXGI_ERROR_DEVICE_REMOVED || c == DXGI_ERROR_DEVICE_RESET {
        DupError::DeviceLost
    } else {
        DupError::Other(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    /// Straight-alpha RGBA.
    pub rgba: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct PointerUpdate {
    /// (x, y, visible) of the top-left of the cursor image in output pixels.
    pub position: Option<(i32, i32, bool)>,
    pub shape: Option<CursorShape>,
}

pub struct Frame {
    /// New desktop image; only valid until [`Duplicator::release`].
    pub image: Option<ID3D11Texture2D>,
    pub pointer: PointerUpdate,
    pub accumulated_frames: u32,
}

pub struct Duplicator {
    dup: IDXGIOutputDuplication,
    pub width: u32,
    pub height: u32,
    /// DXGI_MODE_ROTATION (1 = identity).
    pub rotation: i32,
    holding: bool,
    shape_buf: Vec<u8>,
}

impl Duplicator {
    pub fn new(dev: &D3dDevice, output: &IDXGIOutput) -> Result<Self, DupError> {
        let dup = unsafe {
            // DuplicateOutput1 lets us ask for 8-bit BGRA even on HDR / 10-bit desktops.
            let formats: [DXGI_FORMAT; 1] = [DXGI_FORMAT_B8G8R8A8_UNORM];
            match output.cast::<IDXGIOutput5>() {
                Ok(o5) => match o5.DuplicateOutput1(&dev.device, 0, &formats) {
                    Ok(d) => d,
                    Err(_) => output.cast::<IDXGIOutput1>()?.DuplicateOutput(&dev.device).map_err(classify)?,
                },
                Err(_) => output.cast::<IDXGIOutput1>()?.DuplicateOutput(&dev.device).map_err(classify)?,
            }
        };
        let desc: DXGI_OUTDUPL_DESC = unsafe { dup.GetDesc() };
        Ok(Self {
            dup,
            width: desc.ModeDesc.Width,
            height: desc.ModeDesc.Height,
            rotation: desc.Rotation.0,
            holding: false,
            shape_buf: Vec::new(),
        })
    }

    /// Wait up to `timeout_ms` for a new frame or pointer update.
    pub fn acquire(&mut self, timeout_ms: u32) -> Result<Option<Frame>, DupError> {
        self.release();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        match unsafe { self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut res) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(classify(e)),
        }
        self.holding = true;

        let image = if info.LastPresentTime != 0 {
            res.and_then(|r| r.cast::<ID3D11Texture2D>().ok())
        } else {
            None
        };
        let mut pointer = PointerUpdate::default();
        if info.LastMouseUpdateTime != 0 {
            let p = info.PointerPosition;
            pointer.position = Some((p.Position.x, p.Position.y, p.Visible.as_bool()));
        }
        if info.PointerShapeBufferSize > 0 {
            pointer.shape = self.pointer_shape(info.PointerShapeBufferSize).ok().flatten();
        }
        Ok(Some(Frame { image, pointer, accumulated_frames: info.AccumulatedFrames }))
    }

    /// Release the frame acquired last. Call right after copying the image.
    pub fn release(&mut self) {
        if self.holding {
            unsafe {
                let _ = self.dup.ReleaseFrame();
            }
            self.holding = false;
        }
    }

    fn pointer_shape(&mut self, size: u32) -> windows::core::Result<Option<CursorShape>> {
        self.shape_buf.resize(size as usize, 0);
        let mut required = 0u32;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        unsafe {
            self.dup.GetFramePointerShape(
                size,
                self.shape_buf.as_mut_ptr() as *mut _,
                &mut required,
                &mut info,
            )?;
        }
        Ok(convert_pointer_shape(&info, &self.shape_buf))
    }
}

impl Drop for Duplicator {
    fn drop(&mut self) {
        self.release();
    }
}

const SHAPE_MONOCHROME: u32 = 1;
const SHAPE_COLOR: u32 = 2;
const SHAPE_MASKED_COLOR: u32 = 4;

/// Convert a DXGI pointer shape to straight RGBA. XOR ("invert screen")
/// pixels cannot be expressed in RGBA and are drawn black.
pub fn convert_pointer_shape(info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO, buf: &[u8]) -> Option<CursorShape> {
    let pitch = info.Pitch as usize;
    let (w, h) = match info.Type {
        SHAPE_MONOCHROME => (info.Width, info.Height / 2),
        _ => (info.Width, info.Height),
    };
    if w == 0 || h == 0 {
        return None;
    }
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = (y * w as usize + x) * 4;
            let px: [u8; 4] = match info.Type {
                SHAPE_MONOCHROME => {
                    let bit = 0x80u8 >> (x % 8);
                    let and = buf.get(y * pitch + x / 8)? & bit != 0;
                    let xor = buf.get((y + h as usize) * pitch + x / 8)? & bit != 0;
                    match (and, xor) {
                        (false, false) => [0, 0, 0, 255],
                        (false, true) => [255, 255, 255, 255],
                        (true, false) => [0, 0, 0, 0],
                        (true, true) => [0, 0, 0, 255],
                    }
                }
                SHAPE_COLOR => {
                    let s = buf.get(y * pitch + x * 4..y * pitch + x * 4 + 4)?;
                    [s[2], s[1], s[0], s[3]]
                }
                SHAPE_MASKED_COLOR => {
                    let s = buf.get(y * pitch + x * 4..y * pitch + x * 4 + 4)?;
                    if s[3] == 0 {
                        [s[2], s[1], s[0], 255]
                    } else if s[0] == 0 && s[1] == 0 && s[2] == 0 {
                        [0, 0, 0, 0]
                    } else {
                        [0, 0, 0, 255]
                    }
                }
                _ => return None,
            };
            rgba[o..o + 4].copy_from_slice(&px);
        }
    }
    Some(CursorShape {
        width: w,
        height: h,
        hot_x: info.HotSpot.x,
        hot_y: info.HotSpot.y,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::POINT;

    #[test]
    fn monochrome_shape() {
        // 8x1 cursor: AND row then XOR row.
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: SHAPE_MONOCHROME,
            Width: 8,
            Height: 2,
            Pitch: 1,
            HotSpot: POINT { x: 1, y: 0 },
        };
        let buf = [0b1100_0000u8, 0b1010_0000u8];
        let s = convert_pointer_shape(&info, &buf).unwrap();
        assert_eq!((s.width, s.height, s.hot_x), (8, 1, 1));
        assert_eq!(&s.rgba[0..4], &[0, 0, 0, 255]); // and=1 xor=1 -> black
        assert_eq!(&s.rgba[4..8], &[0, 0, 0, 0]); // and=1 xor=0 -> transparent
        assert_eq!(&s.rgba[8..12], &[255, 255, 255, 255]); // and=0 xor=1 -> white
        assert_eq!(&s.rgba[12..16], &[0, 0, 0, 255]); // and=0 xor=0 -> black
    }

    #[test]
    fn color_shape_swizzles_bgra() {
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO {
            Type: SHAPE_COLOR,
            Width: 1,
            Height: 1,
            Pitch: 4,
            HotSpot: POINT::default(),
        };
        let s = convert_pointer_shape(&info, &[1, 2, 3, 4]).unwrap();
        assert_eq!(s.rgba, vec![3, 2, 1, 4]);
    }
}
