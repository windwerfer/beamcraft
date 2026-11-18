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
    let rgb = |p: &[u8; 4]| {
        (
            p.get(ro).copied().unwrap_or(0) as i32,
            p.get(go).copied().unwrap_or(0) as i32,
            p.get(bo).copied().unwrap_or(0) as i32,
        )
    };
    // Two source rows per chroma row. Rows are sliced once (bounds checked up front), so the
    // inner loops index fixed-size chunks.
    let row_bytes = width * 4;
    for cy in 0..ch {
        let y0 = cy * 2;
        let y1 = (y0 + 1).min(height - 1);
        let (Some(r0), Some(r1)) = (
            src.get(y0 * src_stride..y0 * src_stride + row_bytes),
            src.get(y1 * src_stride..y1 * src_stride + row_bytes),
        ) else {
            bail!("source too small for a {width}x{height} frame");
        };
        for (yy, row) in [(y0, r0), (y1, r1)] {
            if yy == y1 && y1 == y0 {
                continue; // odd height: the last row is its own pair
            }
            let Some(drow) = dst.y.get_mut(yy * dst.y_stride..yy * dst.y_stride + width) else {
                bail!("luma plane too small");
            };
            for (d, p) in drow.iter_mut().zip(row.as_chunks::<4>().0) {
                let (r, g, b) = rgb(p);
                *d = luma(r, g, b);
            }
        }
        let (Some(urow), Some(vrow)) = (
            dst.u.get_mut(cy * dst.uv_stride..cy * dst.uv_stride + cw),
            dst.v.get_mut(cy * dst.uv_stride..cy * dst.uv_stride + cw),
        ) else {
            bail!("chroma plane too small");
        };
        for (cx, (du, dv)) in urow.iter_mut().zip(vrow.iter_mut()).enumerate() {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(width - 1);
            let (mut r, mut g, mut b) = (2, 2, 2);
            for row in [r0, r1] {
                for x in [x0, x1] {
                    let Some(p) = row.as_chunks::<4>().0.get(x) else {
                        continue;
                    };
                    let (pr, pg, pb) = rgb(p);
                    r += pr;
                    g += pg;
                    b += pb;
                }
            }
            (*du, *dv) = chroma(r / 4, g / 4, b / 4);
        }
    }
    Ok(())
}

/// Chroma source of [`yuv_to_rgba`]: separate U and V planes, or one interleaved UV plane (NV12).
pub enum Chroma<'a> {
    Planar {
        u: &'a [u8],
        u_stride: usize,
        v: &'a [u8],
        v_stride: usize,
    },
    Interleaved {
        uv: &'a [u8],
        uv_stride: usize,
    },
}

/// YUV 4:2:0 (BT.601 limited range, nearest chroma) to tightly packed RGBA, the inverse of
/// [`packed_to_i420`]. The viewer's CPU fallback when FFmpeg's swscale is not compiled in.
/// All plane sizes are checked; a short plane is an error.
pub fn yuv_to_rgba(
    width: usize,
    height: usize,
    y: &[u8],
    y_stride: usize,
    chroma: &Chroma<'_>,
    dst: &mut Vec<u8>,
) -> Result<()> {
    dst.clear();
    if width == 0 || height == 0 {
        return Ok(());
    }
    let cw = width.div_ceil(2);
    let short = || anyhow::anyhow!("YUV plane is too short for {width}x{height}");
    dst.reserve(width * height * 4);
    for row in 0..height {
        let yrow = y
            .get(row * y_stride..row * y_stride + width)
            .ok_or_else(short)?;
        let crow = row / 2;
        for (x, &yv) in yrow.iter().enumerate() {
            let cx = x / 2;
            let (u, v) = match chroma {
                Chroma::Planar {
                    u,
                    u_stride,
                    v,
                    v_stride,
                } => (
                    *u.get(crow * u_stride + cx).ok_or_else(short)?,
                    *v.get(crow * v_stride + cx).ok_or_else(short)?,
                ),
                Chroma::Interleaved { uv, uv_stride } => {
                    let i = crow * uv_stride + cx.min(cw - 1) * 2;
                    (
                        *uv.get(i).ok_or_else(short)?,
                        *uv.get(i + 1).ok_or_else(short)?,
                    )
                }
            };
            let c = 298 * (yv as i32 - 16);
            let (d, e) = (u as i32 - 128, v as i32 - 128);
            dst.extend_from_slice(&[
                clamp_u8((c + 409 * e + 128) >> 8),
                clamp_u8((c - 100 * d - 208 * e + 128) >> 8),
                clamp_u8((c + 516 * d + 128) >> 8),
                255,
            ]);
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
    #[cfg(feature = "ffmpeg")]
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

    #[test]
    fn rgba_round_trip_is_close_and_short_planes_error() {
        let (w, h) = (64usize, 48usize);
        let src = picture(w, h);
        let (y, u, v) = convert(&src, w, h, PixelFormat::Rgba);
        let mut out = Vec::new();
        let c = Chroma::Planar {
            u: &u,
            u_stride: w / 2,
            v: &v,
            v_stride: w / 2,
        };
        yuv_to_rgba(w, h, &y, w, &c, &mut out).unwrap();
        assert_eq!(out.len(), w * h * 4);
        // Smooth area (top-left corner, away from the hard block): within chroma-subsampling error.
        for i in [0usize, 1, 2, 5, 6, 7] {
            let d = (out[i] as i32 - src[i] as i32).abs();
            assert!(d <= 12, "channel {i}: {} vs {}", out[i], src[i]);
        }
        assert!(yuv_to_rgba(w, h, &y[..10], w, &c, &mut out).is_err());
    }
}
