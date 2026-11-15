//! FilmCraft's pure-Rust H.264 decoder (`craft-video`): the software fallback when no
//! hardware MediaCodec decoder works (decision log 2026-11-15). It replaced
//! `c2.android.avc.decoder`, which froze on old content after presenter encoder reopens
//! (MTK tablet, 2026-11-15) and took 80–155 ms per 1080p frame; FilmCraft decoded the same
//! recording bit-exact with FFmpeg in 14.8 ms per P-frame on that tablet's Cortex-A76.
//!
//! One thread: frame threading adds a frame or more of latency.

use anyhow::{anyhow, Result};
use craft_video::h264::decode::{Decoder, Picture};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Tightly packed I420 planes (`y`: width × height, `u`/`v`: half size, rounded up).
pub struct Planes {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Planes {
    pub fn chroma_size(&self) -> (u32, u32) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
    }
}

pub struct FilmDecoder {
    inner: Decoder,
    pts: i64,
}

impl Default for FilmDecoder {
    fn default() -> Self {
        Self {
            inner: Decoder::with_threads(1),
            pts: 0,
        }
    }
}

impl FilmDecoder {
    /// Decodes one access unit. A decoder panic (input it does not reject cleanly) becomes an
    /// error and a fresh decoder, so hostile input cannot crash the app.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<Planes>> {
        self.pts += 1;
        let pts = self.pts;
        let inner = &mut self.inner;
        let pictures = match catch_unwind(AssertUnwindSafe(|| inner.decode(au, pts))) {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Err(anyhow!("FilmCraft decode: {e}")),
            Err(_) => {
                self.inner = Decoder::with_threads(1);
                return Err(anyhow!(
                    "FilmCraft decoder panicked on this access unit; restarted it"
                ));
            }
        };
        Ok(pictures.into_iter().last().map(planes))
    }
}

fn planes(p: Picture) -> Planes {
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
    Planes {
        width: p.width,
        height: p.height,
        y: tight(&p.y, p.y_stride, w, h),
        u: tight(&p.u, p.uv_stride, cw, ch),
        v: tight(&p.v, p.uv_stride, cw, ch),
    }
}

/// `rows` rows of `width` bytes out of a strided plane; short planes give mid-grey.
fn tight(plane: &[u8], stride: usize, width: usize, rows: usize) -> Vec<u8> {
    if stride == width && plane.len() >= width * rows {
        return plane[..width * rows].to_vec();
    }
    let mut out = vec![128u8; width * rows];
    for (r, dst) in out.chunks_exact_mut(width.max(1)).enumerate().take(rows) {
        let start = r.saturating_mul(stride);
        if let Some(src) = plane.get(start..start + width) {
            dst.copy_from_slice(src);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error_or_nothing_never_a_panic() {
        let mut d = FilmDecoder::default();
        let mut x = 0x9e37_79b9u32;
        for len in [0usize, 1, 2, 5, 64, 999, 4096] {
            let mut buf = vec![0u8, 0, 0, 1];
            buf.extend((0..len).map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            }));
            let _ = d.decode(&buf);
        }
    }

    #[test]
    fn tight_copies_strided_rows_and_pads_short_planes() {
        assert_eq!(
            tight(&[1, 2, 9, 3, 4, 9, 5], 3, 2, 3),
            vec![1, 2, 3, 4, 128, 128]
        );
    }
}
