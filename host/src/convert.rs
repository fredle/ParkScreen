//! BGRA → YUV 4:2:0 conversion (BT.709, limited range).
//!
//! Both encoders use this so colours match, and the encoder signals BT.709 so the browser
//! decodes with the same matrix. Chroma is the average of each 2x2 block.

/// Converts a BGRA image (`src_stride` bytes per row) of even `w` x `h` into planar I420.
pub fn bgra_to_i420(
    bgra: &[u8],
    src_stride: usize,
    w: usize,
    h: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) {
    debug_assert!(w.is_multiple_of(2) && h.is_multiple_of(2));
    let cw = w / 2;
    for row in 0..h {
        let line = &bgra[row * src_stride..row * src_stride + w * 4];
        let yrow = &mut y[row * w..(row + 1) * w];
        for (px, out) in line.chunks_exact(4).zip(yrow.iter_mut()) {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            *out = (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;
        }
    }
    for crow in 0..h / 2 {
        let l0 = &bgra[(crow * 2) * src_stride..];
        let l1 = &bgra[(crow * 2 + 1) * src_stride..];
        for cx in 0..cw {
            let i = cx * 8;
            let (mut b, mut g, mut r) = (0i32, 0i32, 0i32);
            for off in [i, i + 4] {
                b += l0[off] as i32 + l1[off] as i32;
                g += l0[off + 1] as i32 + l1[off + 1] as i32;
                r += l0[off + 2] as i32 + l1[off + 2] as i32;
            }
            // Average of four pixels: sum / 4, folded into the shift (>> 10 instead of >> 8).
            let uu = ((-26 * r - 87 * g + 112 * b + 512) >> 10) + 128;
            let vv = ((112 * r - 102 * g - 10 * b + 512) >> 10) + 128;
            u[crow * cw + cx] = uu.clamp(0, 255) as u8;
            v[crow * cw + cx] = vv.clamp(0, 255) as u8;
        }
    }
}

/// Converts into NV12 (Y plane followed by interleaved UV), appended to `out` after clearing it.
pub fn bgra_to_nv12(bgra: &[u8], src_stride: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    out.clear();
    out.resize(w * h, 0);
    bgra_to_i420(bgra, src_stride, w, h, &mut out[..], &mut u, &mut v);
    out.reserve(w * h / 2);
    for (a, b) in u.iter().zip(&v) {
        out.push(*a);
        out.push(*b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
        let bgra: Vec<u8> = [b, g, r, 255].repeat(16);
        let (mut y, mut u, mut v) = (vec![0; 16], vec![0; 4], vec![0; 4]);
        bgra_to_i420(&bgra, 16, 4, 4, &mut y, &mut u, &mut v);
        (y[0], u[0], v[0])
    }

    #[test]
    fn black_and_white_are_limited_range() {
        assert_eq!(solid(0, 0, 0), (16, 128, 128));
        let (y, u, v) = solid(255, 255, 255);
        assert!((234..=236).contains(&y), "y={y}");
        assert!((127..=129).contains(&u) && (127..=129).contains(&v));
    }

    #[test]
    fn primaries_match_bt709() {
        let (y, _, v) = solid(0, 0, 255); // red
        assert!((62..=66).contains(&y), "y={y}"); // BT.709 red ≈ 63
        assert!(v > 235, "v={v}");
        let (y, u, _) = solid(255, 0, 0); // blue
        assert!((31..=35).contains(&y), "y={y}"); // ≈ 32
        assert!(u > 235, "u={u}");
    }

    #[test]
    fn honours_source_stride() {
        // 2x2 image inside a 3-pixel-wide buffer; the 3rd column is garbage.
        let mut bgra = vec![0u8; 3 * 4 * 2];
        for row in 0..2 {
            for col in 0..2 {
                let i = (row * 3 + col) * 4;
                bgra[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
            let i = (row * 3 + 2) * 4;
            bgra[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
        let (mut y, mut u, mut v) = (vec![0; 4], vec![0; 1], vec![0; 1]);
        bgra_to_i420(&bgra, 12, 2, 2, &mut y, &mut u, &mut v);
        assert!(y.iter().all(|&p| p > 230));
    }

    #[test]
    fn nv12_interleaves_chroma() {
        let bgra: Vec<u8> = [0, 0, 255, 255].repeat(16);
        let mut out = Vec::new();
        bgra_to_nv12(&bgra, 16, 4, 4, &mut out);
        assert_eq!(out.len(), 16 + 8);
        assert!(out[16..].chunks(2).all(|p| p[0] < 128 && p[1] > 200)); // U low, V high for red
    }
}
