//! Pure-Rust H.264 decoding with FilmCraft's `craft-video` (decision log 2026-11-15): the
//! software decoder on Linux and Windows. macOS keeps FFmpeg's software decoder for now.
//!
//! One worker thread: frame threading pipelines pictures and adds a frame or more of latency,
//! while a single thread returns each picture from the call that fed its access unit (the
//! stream has no B-frames and signals `max_num_reorder_frames = 0`).

use crate::decode::{YuvImage, YuvLayout};
use anyhow::{anyhow, Result};
use craft_video::h264::decode::{Decoder as CraftDecoder, Picture};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub struct FilmDecoder {
    inner: CraftDecoder,
    pts: i64,
    name: String,
}

impl FilmDecoder {
    /// `why` says why this software decoder runs (shown in the viewer overlay).
    pub fn new(why: &str) -> Self {
        Self {
            inner: CraftDecoder::with_threads(1),
            pts: 0,
            name: format!("FilmCraft h264 (software, pure Rust: {why})"),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Decodes one access unit. A panic inside the decoder (malformed input it does not
    /// reject cleanly) becomes an error and a fresh decoder, never a crashed viewer.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<YuvImage>> {
        self.pts += 1;
        let pts = self.pts;
        let inner = &mut self.inner;
        let result = catch_unwind(AssertUnwindSafe(|| inner.decode(au, pts)));
        let pictures = match result {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return Err(anyhow!("FilmCraft decode: {e}")),
            Err(_) => {
                self.inner = CraftDecoder::with_threads(1);
                return Err(anyhow!(
                    "FilmCraft decoder panicked on this access unit; restarted it"
                ));
            }
        };
        // Low delay: at most one picture per access unit; keep the newest if several arrive.
        Ok(pictures.into_iter().last().map(to_yuv))
    }
}

fn to_yuv(p: Picture) -> YuvImage {
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
    YuvImage {
        width: p.width,
        height: p.height,
        layout: YuvLayout::Yuv420,
        y: tight(&p.y, p.y_stride, w, h),
        u: tight(&p.u, p.uv_stride, cw, ch),
        v: tight(&p.v, p.uv_stride, cw, ch),
        y_stride: w,
        u_stride: cw,
        v_stride: cw,
    }
}

/// Copies `rows` rows of `width` bytes out of a strided plane (no copy when already tight).
/// Short planes are padded with mid-grey instead of panicking.
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
        let mut d = FilmDecoder::new("test");
        let mut x = 0x1234_5678u32;
        for len in [0usize, 1, 3, 4, 17, 300, 5000] {
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
        let plane = [1u8, 2, 9, 3, 4, 9, 5];
        assert_eq!(tight(&plane, 3, 2, 3), vec![1, 2, 3, 4, 128, 128]);
        assert_eq!(tight(&[7u8; 4], 2, 2, 2), vec![7; 4]);
    }
}
