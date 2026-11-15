//! Desktop adapter for FilmCraft's pure-Rust H.264 decoder (`beamcraft-film`, decision log
//! 2026-11-15): the software decoder on Linux and Windows. macOS keeps FFmpeg's software
//! decoder for now. All FilmCraft API use is in `beamcraft-film`; this only maps its planes
//! to [`YuvImage`] and names the decoder for the overlay.

use crate::decode::{YuvImage, YuvLayout};
use anyhow::Result;
use beamcraft_film::Planes;

pub struct FilmDecoder {
    inner: beamcraft_film::FilmDecoder,
    name: String,
}

impl FilmDecoder {
    /// `why` says why this software decoder runs (shown in the viewer overlay).
    pub fn new(why: &str) -> Self {
        Self {
            inner: beamcraft_film::FilmDecoder::default(),
            name: format!("FilmCraft h264 (software, pure Rust: {why})"),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Decodes one access unit; malformed input is an error, never a panic.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<YuvImage>> {
        Ok(self.inner.decode(au)?.map(to_yuv))
    }
}

/// `Planes` are tightly packed, so each stride is the plane width (no copy).
fn to_yuv(p: Planes) -> YuvImage {
    let (cw, _) = p.chroma_size();
    let (w, cw) = (p.width as usize, cw as usize);
    YuvImage {
        width: p.width,
        height: p.height,
        layout: YuvLayout::Yuv420,
        y: p.y,
        u: p.u,
        v: p.v,
        y_stride: w,
        u_stride: cw,
        v_stride: cw,
    }
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
    fn planes_map_to_tight_strides() {
        let p = Planes {
            width: 3,
            height: 2,
            y: vec![0; 6],
            u: vec![0; 2],
            v: vec![0; 2],
        };
        let y = to_yuv(p);
        assert_eq!((y.y_stride, y.u_stride, y.v_stride), (3, 2, 2));
    }
}
