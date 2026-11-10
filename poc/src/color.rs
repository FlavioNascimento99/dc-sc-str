pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;

pub fn nv12_len() -> usize {
    (WIDTH as usize) * (HEIGHT as usize) * 3 / 2
}

pub fn bgra_len() -> usize {
    (WIDTH as usize) * (HEIGHT as usize) * 4
}

/// BT.601 limited-range BGRA to NV12. Width and height must be even.
pub fn bgra_to_nv12(bgra: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let stride = stride as usize;
    let mut out = vec![0u8; w * h * 3 / 2];
    let y_size = w * h;

    for y in 0..h {
        let row = y * stride;
        for x in 0..w {
            let i = row + x * 4;
            let b = bgra[i] as i32;
            let g = bgra[i + 1] as i32;
            let r = bgra[i + 2] as i32;
            let yy = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
            out[y * w + x] = yy.clamp(0, 255) as u8;
        }
    }

    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let mut r = 0;
            let mut g = 0;
            let mut b = 0;
            for dy in 0..2 {
                let row = (y + dy) * stride;
                for dx in 0..2 {
                    let i = row + (x + dx) * 4;
                    b += bgra[i] as i32;
                    g += bgra[i + 1] as i32;
                    r += bgra[i + 2] as i32;
                }
            }
            r /= 4;
            g /= 4;
            b /= 4;
            let u = ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
            let v = ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
            let uv = y_size + (y / 2) * w + x;
            out[uv] = u.clamp(0, 255) as u8;
            out[uv + 1] = v.clamp(0, 255) as u8;
        }
    }

    out
}

pub fn nv12_to_bgra(
    nv12: &[u8],
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let y_stride = y_stride as usize;
    let uv_stride = uv_stride as usize;
    let mut out = vec![0u8; w * h * 4];
    let uv_base = y_stride * h;

    for y in 0..h {
        for x in 0..w {
            let yy = nv12[y * y_stride + x] as i32;
            let uv = uv_base + (y / 2) * uv_stride + (x & !1);
            let u = nv12[uv] as i32 - 128;
            let v = nv12[uv + 1] as i32 - 128;
            let c = yy - 16;
            let r = ((298 * c + 409 * v + 128) >> 8).clamp(0, 255);
            let g = ((298 * c - 100 * u - 208 * v + 128) >> 8).clamp(0, 255);
            let b = ((298 * c + 516 * u + 128) >> 8).clamp(0, 255);
            let o = (y * w + x) * 4;
            out[o] = b as u8;
            out[o + 1] = g as u8;
            out[o + 2] = r as u8;
            out[o + 3] = 255;
        }
    }

    out
}

/// Media Foundation may emit Annex-B or 4-byte length-prefixed NALs.
pub fn to_annex_b(data: &[u8]) -> Vec<u8> {
    if data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]) {
        return data.to_vec();
    }

    let mut out = Vec::with_capacity(data.len() + 16);
    let mut i = 0;
    let mut ok = !data.is_empty();
    while i + 4 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        if len == 0 || i + len > data.len() {
            ok = false;
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + len]);
        i += len;
    }
    if ok && i == data.len() && !out.is_empty() {
        return out;
    }

    let mut single = Vec::with_capacity(data.len() + 4);
    single.extend_from_slice(&[0, 0, 0, 1]);
    single.extend_from_slice(data);
    single
}

#[allow(dead_code)]
pub fn is_keyframe(annex_b: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 < annex_b.len() {
        let start = if annex_b[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if annex_b[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let nal = i + start;
        if nal < annex_b.len() && annex_b[nal] & 0x1f == 5 {
            return true;
        }
        i = nal + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_pixel_is_limited_range_yuv() {
        let mut bgra = vec![0u8; 2 * 2 * 4];
        for px in bgra.chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 255, 255]);
        }
        let nv12 = bgra_to_nv12(&bgra, 2, 2, 8);
        assert_eq!(nv12.len(), 6);
        assert!(nv12[0] > 70 && nv12[0] < 100);
        assert!(nv12[4] > 80 && nv12[4] < 110);
        assert!(nv12[5] > 220);
    }

    #[test]
    fn length_prefixed_nals_become_annex_b() {
        let data = [0u8, 0, 0, 2, 0x65, 0x01, 0, 0, 0, 1, 0x09];
        let out = to_annex_b(&data);
        assert_eq!(out, vec![0, 0, 0, 1, 0x65, 0x01, 0, 0, 0, 1, 0x09]);
        let already = to_annex_b(&[0, 0, 0, 1, 0x65]);
        assert_eq!(already, vec![0, 0, 0, 1, 0x65]);
    }

    #[test]
    fn idr_is_keyframe() {
        assert!(is_keyframe(&[0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65, 1]));
        assert!(!is_keyframe(&[0, 0, 0, 1, 0x41, 1, 2]));
    }
}
