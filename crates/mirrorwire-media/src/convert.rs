//! Pure-Rust packed 32-bit RGB → I420 (YUV 4:2:0 planar) conversion.
//!
//! Scalar, safe, BT.601 limited range: the same picture swscale produces by default for BGRx →
//! YUV420P. It exists so an FFmpeg-free build (cargo feature `ffmpeg` off) can still feed the
//! FilmCraft encoder; where FFmpeg is on, swscale stays the default converter (decisions.md D17).
//! Chroma is the average of each 2×2 block (clamped at odd edges).

use crate::frame::PixelFormat;
use anyhow::{bail, Result};

/// Destination planes of an I420 picture. Chroma planes are `(w+1)/2 × (h+1)/2`.
pub struct I420Planes<'a> {
    pub y: &'a mut [u8],
    pub u: &'a mut [u8],
    pub v: &'a mut [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// `(r, g, b)` byte offsets inside one 4-byte pixel.
fn rgb_offsets(format: PixelFormat) -> (usize, usize, usize) {
    match format {
        PixelFormat::Bgra | PixelFormat::Bgrx => (2, 1, 0),
        PixelFormat::Rgba | PixelFormat::Rgbx => (0, 1, 2),
    }
}

fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BT.601 limited range, 8-bit fixed point (coefficients scaled by 256).
fn luma(r: i32, g: i32, b: i32) -> u8 {
    clamp_u8(16 + ((66 * r + 129 * g + 25 * b + 128) >> 8))
}

fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    (
        clamp_u8(128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)),
        clamp_u8(128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)),
    )
}

/// Convert a packed frame (`src`, row pitch `src_stride`) to I420. All slice sizes are checked;
/// a too-small buffer is an error, never a panic.
pub fn packed_to_i420(
    src: &[u8],
    src_stride: usize,
    width: usize,
    height: usize,
    format: PixelFormat,
    dst: &mut I420Planes<'_>,
) -> Result<()> {
    if width == 0 || height == 0 {
        return Ok(());
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    if src_stride < width * 4
        || src.len() < src_stride * (height - 1) + width * 4
        || dst.y_stride < width
        || dst.uv_stride < cw
        || dst.y.len() < dst.y_stride * (height - 1) + width
        || dst.u.len() < dst.uv_stride * (ch - 1) + cw
        || dst.v.len() < dst.uv_stride * (ch - 1) + cw
    {
        bail!("buffer too small for a {width}x{height} I420 conversion");
    }
    let (ro, go, bo) = rgb_offsets(format);
    let px = |x: usize, y: usize| -> (i32, i32, i32) {
        let i = y * src_stride + x * 4;
        match src.get(i..i + 4) {
            Some(p) => (
                p.get(ro).copied().unwrap_or(0) as i32,
                p.get(go).copied().unwrap_or(0) as i32,
                p.get(bo).copied().unwrap_or(0) as i32,
            ),
            None => (0, 0, 0),
        }
    };
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = px(x, y);
            if let Some(d) = dst.y.get_mut(y * dst.y_stride + x) {
                *d = luma(r, g, b);
            }
        }
    }
    for cy in 0..ch {
        for cx in 0..cw {
            let (mut r, mut g, mut b) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (pr, pg, pb) = px((cx * 2 + dx).min(width - 1), (cy * 2 + dy).min(height - 1));
                r += pr;
                g += pg;
                b += pb;
            }
            let (u, v) = chroma((r + 2) / 4, (g + 2) / 4, (b + 2) / 4);
            let i = cy * dst.uv_stride + cx;
            if let Some(d) = dst.u.get_mut(i) {
                *d = u;
            }
            if let Some(d) = dst.v.get_mut(i) {
                *d = v;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smooth gradients plus a hard-edged block; deterministic.
    fn picture(w: usize, h: usize) -> Vec<u8> {
        let mut d = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let block = (20..40).contains(&x) && (10..30).contains(&y);
                d[i] = if block { 250 } else { (x * 255 / w) as u8 };
                d[i + 1] = if block { 10 } else { (y * 255 / h) as u8 };
                d[i + 2] = if block {
                    30
                } else {
                    ((x + y) * 255 / (w + h)) as u8
                };
                d[i + 3] = 255;
            }
        }
        d
    }

    fn convert(src: &[u8], w: usize, h: usize, fmt: PixelFormat) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (mut y, mut u, mut v) = (vec![0; w * h], vec![0; cw * ch], vec![0; cw * ch]);
        let mut p = I420Planes {
            y: &mut y,
            u: &mut u,
            v: &mut v,
            y_stride: w,
            uv_stride: cw,
        };
        packed_to_i420(src, w * 4, w, h, fmt, &mut p).unwrap();
        (y, u, v)
    }

    #[test]
    fn known_colours() {
        // White, black, pure red in BGRA.
        let white = convert(&[255; 16], 2, 2, PixelFormat::Bgra);
        assert_eq!((white.0[0], white.1[0], white.2[0]), (235, 128, 128));
        let black = convert(&[0, 0, 0, 255].repeat(4), 2, 2, PixelFormat::Bgra);
        assert_eq!((black.0[0], black.1[0], black.2[0]), (16, 128, 128));
        let red = convert(&[0, 0, 255, 255].repeat(4), 2, 2, PixelFormat::Bgra);
        assert!((i32::from(red.0[0]) - 82).abs() <= 1);
        assert!((i32::from(red.1[0]) - 90).abs() <= 1);
        assert!((i32::from(red.2[0]) - 240).abs() <= 1);
    }

    #[test]
    fn rgba_and_bgra_agree_after_swizzle() {
        let (w, h) = (16, 8);
        let bgra = picture(w, h);
        let mut rgba = bgra.clone();
        for p in rgba.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        assert_eq!(
            convert(&bgra, w, h, PixelFormat::Bgra),
            convert(&rgba, w, h, PixelFormat::Rgba)
        );
    }

    #[test]
    fn odd_sizes_and_short_buffers() {
        let src = picture(7, 5);
        let (y, u, v) = convert(&src, 7, 5, PixelFormat::Bgrx);
        assert_eq!((y.len(), u.len(), v.len()), (35, 12, 12));
        let (mut yy, mut uu, mut vv) = (vec![0; 4], vec![0; 1], vec![0; 1]);
        let mut p = I420Planes {
            y: &mut yy,
            u: &mut uu,
            v: &mut vv,
            y_stride: 2,
            uv_stride: 1,
        };
        assert!(packed_to_i420(&src, 28, 7, 5, PixelFormat::Bgrx, &mut p).is_err());
        assert!(packed_to_i420(&src[..10], 28, 2, 2, PixelFormat::Bgrx, &mut p).is_err());
    }

    /// Within a small tolerance of swscale's BGRA → YUV420P.
    #[test]
    fn matches_swscale() {
        use ffmpeg_next::{format::Pixel, frame::Video, software::scaling};
        crate::init().unwrap();
        let (w, h) = (64usize, 48usize);
        let src = picture(w, h);
        let mut inp = Video::new(Pixel::BGRA, w as u32, h as u32);
        let s = inp.stride(0);
        for row in 0..h {
            inp.data_mut(0)[row * s..row * s + w * 4].copy_from_slice(&src[row * w * 4..][..w * 4]);
        }
        let mut ctx = scaling::Context::get(
            Pixel::BGRA,
            w as u32,
            h as u32,
            Pixel::YUV420P,
            w as u32,
            h as u32,
            scaling::Flags::BILINEAR,
        )
        .unwrap();
        let mut out = Video::empty();
        ctx.run(&inp, &mut out).unwrap();
        let (y, u, v) = convert(&src, w, h, PixelFormat::Bgra);
        let cmp = |plane: usize, mine: &[u8], mw: usize, mh: usize, tol: i32| {
            let st = out.stride(plane);
            let mut worst = 0;
            for r in 0..mh {
                for c in 0..mw {
                    let d = (i32::from(out.data(plane)[r * st + c]) - i32::from(mine[r * mw + c]))
                        .abs();
                    worst = worst.max(d);
                }
            }
            assert!(worst <= tol, "plane {plane}: max diff {worst} > {tol}");
        };
        cmp(0, &y, w, h, 3);
        // Chroma differs more at hard edges (swscale's chroma siting/filter).
        cmp(1, &u, w / 2, h / 2, 20);
        cmp(2, &v, w / 2, h / 2, 20);
    }
}
