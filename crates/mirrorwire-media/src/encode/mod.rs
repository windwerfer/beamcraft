use crate::frame::{EncodedFrame, RawFrame};
use anyhow::{anyhow, bail, Result};

mod config;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
#[cfg(feature = "ffmpeg")]
mod rc;
#[cfg(feature = "ffmpeg")]
mod vaapi;
pub use config::*;
#[cfg(feature = "ffmpeg")]
use ffmpeg::FfmpegEncoder;

/// The encoder could not use a DMA-BUF frame. The capture should switch to
/// shared memory (`FrameSource::disable_dmabuf`); the frame is dropped.
#[derive(Debug)]
pub struct DmaBufError(pub String);

impl std::fmt::Display for DmaBufError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DmaBufError {}

/// Capture/convert path labels shown in the presenter status and log.
pub mod path {
    /// Compositor GPU buffer imported into VAAPI, converted by VPP, encoded. No CPU copy.
    pub const DMABUF_ZERO_COPY: &str = "dmabuf-zero-copy";
    /// Shared-memory frame uploaded to the GPU, converted by VAAPI VPP.
    pub const SHM_VPP: &str = "shm+upload (vaapi-vpp)";
    /// Shared-memory frame converted to NV12 on the CPU, then uploaded.
    pub const SHM_CPU_UPLOAD: &str = "shm+upload (cpu convert)";
    /// Software encoder: CPU conversion, no GPU.
    pub const SHM_CPU: &str = "shm (cpu)";
}

/// Low-latency H.264 encoder. The FFmpeg backends (libx264, VAAPI, VideoToolbox) exist only with
/// cargo feature `ffmpeg`; FilmCraft (feature `filmcraft`) is pure Rust. Everything outside this
/// module sees one type.
pub enum Encoder {
    #[cfg(feature = "ffmpeg")]
    Ffmpeg(Box<FfmpegEncoder>),
    #[cfg(feature = "filmcraft")]
    Film(Box<crate::filmcraft::FilmEncoder>),
}

/// Readable text for "no H.264 encoder could be opened": every backend's reason, plus how to
/// get one. Shown in the GUI, so it names the fix instead of only an FFmpeg error.
fn no_encoder_message(failures: &[(EncoderBackend, String)]) -> String {
    let mut msg = String::from("No H.264 encoder could be started.");
    for (backend, why) in failures {
        msg.push_str(&format!(" {backend:?}: {why}."));
    }
    let x264_missing = failures
        .iter()
        .any(|(b, w)| *b == EncoderBackend::X264 && w.contains("not built into this FFmpeg"));
    if x264_missing && cfg!(target_os = "linux") {
        msg.push_str(
            " This FFmpeg has no libx264 (Fedora's ffmpeg-free lacks it). Install the full \
             FFmpeg: enable RPM Fusion and run `sudo dnf swap ffmpeg-free ffmpeg --allowerasing`, \
             or on Debian/Ubuntu `sudo apt install ffmpeg`; in Flatpak install the \
             org.freedesktop.Platform.ffmpeg-full extension. A working VA-API driver also works.",
        );
    }
    msg
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if config.width == 0 || config.height == 0 {
            bail!("encoder size is not resolved yet");
        }
        let order: &[EncoderBackend] = match config.backend {
            EncoderBackend::Auto if cfg!(target_os = "macos") => {
                &[EncoderBackend::VideoToolbox, EncoderBackend::X264]
            }
            EncoderBackend::Auto => &[EncoderBackend::Vaapi, EncoderBackend::X264],
            EncoderBackend::Vaapi => &[EncoderBackend::Vaapi],
            EncoderBackend::X264 => &[EncoderBackend::X264],
            EncoderBackend::VideoToolbox => &[EncoderBackend::VideoToolbox],
            EncoderBackend::Filmcraft => &[EncoderBackend::Filmcraft],
        };
        let mut failures: Vec<(EncoderBackend, String)> = Vec::new();
        for &backend in order {
            match Self::open(&config, backend) {
                Ok(enc) => return Ok(enc),
                Err(e) => {
                    tracing::warn!("encoder {backend:?} unavailable: {e:#}");
                    failures.push((backend, format!("{e:#}")));
                }
            }
        }
        Err(anyhow!(no_encoder_message(&failures)))
    }

    fn open(config: &EncoderConfig, backend: EncoderBackend) -> Result<Self> {
        if backend == EncoderBackend::Filmcraft {
            return Self::open_filmcraft(config);
        }
        Self::open_ffmpeg(config, backend)
    }

    #[cfg(feature = "ffmpeg")]
    fn open_ffmpeg(config: &EncoderConfig, backend: EncoderBackend) -> Result<Self> {
        Ok(Self::Ffmpeg(Box::new(FfmpegEncoder::open(
            config, backend,
        )?)))
    }

    #[cfg(not(feature = "ffmpeg"))]
    fn open_ffmpeg(_config: &EncoderConfig, _backend: EncoderBackend) -> Result<Self> {
        bail!("this build has no FFmpeg (cargo feature `ffmpeg`); use --encoder filmcraft")
    }

    #[cfg(feature = "filmcraft")]
    fn open_filmcraft(config: &EncoderConfig) -> Result<Self> {
        Ok(Self::Film(Box::new(crate::filmcraft::FilmEncoder::new(
            config,
        )?)))
    }

    #[cfg(not(feature = "filmcraft"))]
    fn open_filmcraft(_config: &EncoderConfig) -> Result<Self> {
        bail!("this build has no FilmCraft encoder (cargo feature `filmcraft`)")
    }
}

/// Forward a `&self`/`&mut self` method to whichever backend is active.
macro_rules! dispatch {
    ($self:ident, $e:ident => $body:expr) => {
        match $self {
            #[cfg(feature = "ffmpeg")]
            Self::Ffmpeg($e) => $body,
            #[cfg(feature = "filmcraft")]
            Self::Film($e) => $body,
        }
    };
}

impl Encoder {
    pub fn name(&self) -> &'static str {
        dispatch!(self, e => e.name())
    }

    pub fn conversion_path(&self) -> &'static str {
        dispatch!(self, e => e.conversion_path())
    }

    pub fn rc_summary(&self) -> &str {
        dispatch!(self, e => e.rc_summary())
    }

    pub fn output_size(&self) -> (u32, u32) {
        dispatch!(self, e => e.output_size())
    }

    pub fn config(&self) -> &EncoderConfig {
        dispatch!(self, e => e.config())
    }

    /// Whether [`Self::set_bitrate_live`] works: only libx264 in a bitrate mode re-reads its rate
    /// settings per frame. VAAPI, VideoToolbox and FilmCraft only read them when opened.
    pub fn supports_live_bitrate(&self) -> bool {
        match self {
            #[cfg(feature = "ffmpeg")]
            Self::Ffmpeg(e) => e.supports_live_bitrate(),
            #[cfg(feature = "filmcraft")]
            Self::Film(_) => false,
        }
    }

    /// Change the target bitrate without reopening the encoder (no keyframe). Returns `false`
    /// (nothing changed) when the encoder can't; the caller reopens it instead.
    pub fn set_bitrate_live(&mut self, kbps: u32) -> bool {
        match self {
            #[cfg(feature = "ffmpeg")]
            Self::Ffmpeg(e) => e.set_bitrate_live(kbps),
            #[cfg(feature = "filmcraft")]
            Self::Film(_) => {
                let _ = kbps;
                false
            }
        }
    }

    /// (convert, encode) in milliseconds.
    pub fn last_timings_ms(&self) -> (f64, f64) {
        dispatch!(self, e => e.last_timings_ms())
    }

    /// pack / colour-convert / upload / encode, in milliseconds.
    pub fn last_breakdown_ms(&self) -> (f64, f64, f64, f64) {
        dispatch!(self, e => e.last_breakdown_ms())
    }

    /// Whether this encoder can take DMA-BUF frames (VAAPI only).
    pub fn accepts_dmabuf(&self) -> bool {
        match self {
            #[cfg(feature = "ffmpeg")]
            Self::Ffmpeg(e) => e.accepts_dmabuf(),
            #[cfg(feature = "filmcraft")]
            Self::Film(_) => false,
        }
    }

    /// Why the VPP or DMA-BUF path was dropped, for the status line.
    pub fn fallback_note(&self) -> Option<&str> {
        match self {
            #[cfg(feature = "ffmpeg")]
            Self::Ffmpeg(e) => e.fallback_note(),
            #[cfg(feature = "filmcraft")]
            Self::Film(_) => None,
        }
    }

    pub fn encode(&mut self, raw: &RawFrame, force_keyframe: bool) -> Result<Vec<EncodedFrame>> {
        dispatch!(self, e => e.encode(raw, force_keyframe))
    }

    /// Encode the last converted frame again with a new timestamp. Used when the compositor only
    /// sends frames on damage, so the viewer keeps getting a steady stream. Returns nothing
    /// before the first real frame.
    pub fn encode_repeat(
        &mut self,
        seq: u64,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        dispatch!(self, e => e.encode_repeat(seq, capture_us, force_keyframe))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_accepts_x_and_times() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        assert_eq!(parse_size("1920×1080").unwrap(), (1920, 1080));
    }

    #[test]
    fn resolved_size_defaults_to_capture() {
        let mut c = EncoderConfig::default();
        assert_eq!(c.resolved_size(1920, 1080), (1920, 1080));
        c.width = 1280;
        c.height = 720;
        assert_eq!(c.resolved_size(1920, 1080), (1280, 720));
    }

    #[test]
    fn resolved_size_keeps_aspect_is_even_and_bounded() {
        let mut c = EncoderConfig::default();
        assert_eq!(c.resolved_size(1919, 1081), (1918, 1080));
        c.height = 720;
        assert_eq!(c.resolved_size(1920, 1080), (1280, 720));
        c.height = 0;
        c.width = 1000;
        assert_eq!(c.resolved_size(1920, 1080), (1000, 562));
        c.width = 0;
        assert_eq!(c.resolved_size(5120, 2880), (4096, 2304));
        assert_eq!(c.resolved_size(7680, 4320), (4096, 2304));
        let (w, h) = c.resolved_size(5120, 2160);
        assert!(w <= 4096 && u64::from(w) * u64::from(h) <= MAX_ENCODE_PIXELS);
        assert!(w % 2 == 0 && h % 2 == 0);
        assert_eq!(c.resolved_size(3840, 2160), (3840, 2160));
        assert_eq!(c.resolved_size(0, 0), (2, 2));
    }

    #[test]
    fn default_bitrate_is_forty_mbps() {
        assert_eq!(DEFAULT_BITRATE_KBPS, 40_000);
        assert_eq!(EncoderConfig::default().bitrate_kbps, 40_000);
        assert_eq!(EncoderConfig::default().width, 0);
    }
}

#[cfg(test)]
mod no_encoder_tests {
    use super::*;

    #[test]
    fn message_lists_every_backend_and_the_fix() {
        let m = no_encoder_message(&[
            (EncoderBackend::Vaapi, "no VA-API device".into()),
            (
                EncoderBackend::X264,
                "libx264 not built into this FFmpeg".into(),
            ),
        ]);
        assert!(m.contains("Vaapi: no VA-API device"));
        assert!(m.contains("X264: libx264 not built"));
        if cfg!(target_os = "linux") {
            assert!(m.contains("dnf swap ffmpeg-free"));
        }
    }

    #[test]
    fn other_failures_get_no_install_hint() {
        let m = no_encoder_message(&[(EncoderBackend::X264, "out of memory".into())]);
        assert!(!m.contains("dnf"));
    }
}
