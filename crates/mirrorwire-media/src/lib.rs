// Every `unsafe` block or impl needs a `// SAFETY:` comment (AGENTS.md §1.12, decisions D15).
// Panicking shortcuts are allowed in tests only (AGENTS.md 1.9, clippy.toml).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![deny(clippy::undocumented_unsafe_blocks)]
// `unsafe` is allowed only in decode, encode, hw and source::portal (FFmpeg/VA-API FFI, one raw fd).
#![deny(unsafe_code)]
#[allow(unsafe_code)]
pub mod decode;
#[allow(unsafe_code)]
pub mod encode;
pub mod film_decode;
#[cfg(feature = "filmcraft")]
pub mod filmcraft;
pub mod frame;
#[allow(unsafe_code)]
mod hw;
pub mod pattern;
pub mod pipeline;
pub mod source;

pub fn init() -> anyhow::Result<()> {
    ffmpeg_next::init()?;
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    Ok(())
}
