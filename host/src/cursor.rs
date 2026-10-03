//! Compositing the mouse pointer onto captured frames.
//!
//! Desktop Duplication frames exclude the pointer; it is reported separately as a shape
//! (one of three formats) plus a position. This module draws it into a BGRA buffer.

/// DXGI_OUTDUPL_POINTER_SHAPE_TYPE values.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;

#[derive(Debug, Clone, Default)]
pub struct CursorShape {
    pub kind: u32,
    pub width: u32,
    /// For monochrome shapes this is already halved (the AND mask plus the XOR mask
    /// are stored one after the other, so the raw height is twice the visible height).
    pub height: u32,
    pub pitch: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorState {
    pub visible: bool,
    /// Top-left of the shape in frame pixels (the hotspot offset is already applied).
    pub x: i32,
    pub y: i32,
}

/// Draw `shape` at `state` into a tightly packed BGRA frame of `fw` x `fh`.
pub fn composite(frame: &mut [u8], fw: usize, fh: usize, shape: &CursorShape, state: CursorState) {
    if !state.visible || shape.width == 0 || shape.height == 0 {
        return;
    }
    for sy in 0..shape.height as i32 {
        let fy = state.y + sy;
        if fy < 0 || fy >= fh as i32 {
            continue;
        }
        for sx in 0..shape.width as i32 {
            let fx = state.x + sx;
            if fx < 0 || fx >= fw as i32 {
                continue;
            }
            let dst = (fy as usize * fw + fx as usize) * 4;
            let px = &mut frame[dst..dst + 4];
            match shape.kind {
                SHAPE_MONOCHROME => mono_pixel(px, shape, sx as usize, sy as usize),
                SHAPE_COLOR => {
                    let s = sy as usize * shape.pitch as usize + sx as usize * 4;
                    if let Some(src) = shape.data.get(s..s + 4) {
                        let a = src[3] as u32;
                        for c in 0..3 {
                            px[c] = ((src[c] as u32 * a + px[c] as u32 * (255 - a) + 127) / 255) as u8;
                        }
                    }
                }
                SHAPE_MASKED_COLOR => {
                    let s = sy as usize * shape.pitch as usize + sx as usize * 4;
                    if let Some(src) = shape.data.get(s..s + 4) {
                        // Alpha is a mask: 0 replaces the pixel, 0xFF XORs it.
                        for c in 0..3 {
                            px[c] = if src[3] == 0 { src[c] } else { px[c] ^ src[c] };
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn mono_pixel(px: &mut [u8], shape: &CursorShape, sx: usize, sy: usize) {
    let pitch = shape.pitch as usize;
    let bit = 0x80u8 >> (sx % 8);
    let and_idx = sy * pitch + sx / 8;
    let xor_idx = (sy + shape.height as usize) * pitch + sx / 8;
    let (Some(&and_b), Some(&xor_b)) = (shape.data.get(and_idx), shape.data.get(xor_idx)) else {
        return;
    };
    let and = if and_b & bit != 0 { 0xFF } else { 0x00 };
    let xor = if xor_b & bit != 0 { 0xFF } else { 0x00 };
    for p in px.iter_mut().take(3) {
        *p = (*p & and) ^ xor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, v: u8) -> Vec<u8> {
        vec![v; w * h * 4]
    }

    fn px(f: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [f[i], f[i + 1], f[i + 2], f[i + 3]]
    }

    #[test]
    fn color_alpha_blends() {
        let mut f = frame(4, 4, 0);
        let shape = CursorShape {
            kind: SHAPE_COLOR,
            width: 1,
            height: 1,
            pitch: 4,
            data: vec![200, 100, 50, 255],
        };
        composite(&mut f, 4, 4, &shape, CursorState { visible: true, x: 2, y: 1 });
        assert_eq!(px(&f, 4, 2, 1)[..3], [200, 100, 50]);
        assert_eq!(px(&f, 4, 1, 1)[..3], [0, 0, 0]);

        // 50% alpha over mid-grey
        let mut g = frame(2, 1, 100);
        let half = CursorShape { data: vec![200, 200, 200, 128], ..shape.clone() };
        composite(&mut g, 2, 1, &half, CursorState { visible: true, x: 0, y: 0 });
        let v = px(&g, 2, 0, 0)[0];
        assert!((149..=151).contains(&v), "got {v}");
    }

    #[test]
    fn masked_color_replaces_or_xors() {
        let mut f = frame(2, 1, 0b1010_1010);
        let shape = CursorShape {
            kind: SHAPE_MASKED_COLOR,
            width: 2,
            height: 1,
            pitch: 8,
            // pixel 0: mask 0 -> replace with (1,2,3); pixel 1: mask FF -> xor with 0xFF
            data: vec![1, 2, 3, 0, 0xFF, 0xFF, 0xFF, 0xFF],
        };
        composite(&mut f, 2, 1, &shape, CursorState { visible: true, x: 0, y: 0 });
        assert_eq!(px(&f, 2, 0, 0)[..3], [1, 2, 3]);
        assert_eq!(px(&f, 2, 1, 0)[..3], [0b0101_0101; 3]);
    }

    #[test]
    fn monochrome_and_xor_masks() {
        // 8x1 cursor: AND row then XOR row, 1 byte each.
        // bit7: and=1 xor=0 -> keep; bit6: and=0 xor=0 -> black;
        // bit5: and=0 xor=1 -> white; bit4: and=1 xor=1 -> invert.
        let shape = CursorShape {
            kind: SHAPE_MONOCHROME,
            width: 8,
            height: 1,
            pitch: 1,
            data: vec![0b1001_0000, 0b0011_0000],
        };
        let mut f = frame(8, 1, 0x40);
        composite(&mut f, 8, 1, &shape, CursorState { visible: true, x: 0, y: 0 });
        assert_eq!(px(&f, 8, 0, 0)[0], 0x40);
        assert_eq!(px(&f, 8, 1, 0)[0], 0x00);
        assert_eq!(px(&f, 8, 2, 0)[0], 0xFF);
        assert_eq!(px(&f, 8, 3, 0)[0], !0x40u8);
    }

    #[test]
    fn clips_at_edges_and_respects_visibility() {
        let shape = CursorShape {
            kind: SHAPE_COLOR,
            width: 3,
            height: 3,
            pitch: 12,
            data: [9, 9, 9, 255].repeat(9),
        };
        let mut f = frame(4, 4, 0);
        composite(&mut f, 4, 4, &shape, CursorState { visible: true, x: -2, y: 3 });
        assert_eq!(px(&f, 4, 0, 3)[0], 9);
        assert_eq!(px(&f, 4, 1, 3)[0], 0);
        let mut g = frame(4, 4, 0);
        composite(&mut g, 4, 4, &shape, CursorState { visible: false, x: 0, y: 0 });
        assert!(g.iter().all(|&b| b == 0));
        composite(&mut g, 4, 4, &shape, CursorState { visible: true, x: 100, y: 100 });
        assert!(g.iter().all(|&b| b == 0));
    }
}
