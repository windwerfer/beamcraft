use anyhow::{anyhow, bail, Result};

/// Default target bitrate for desktop content. Per-frame bits are based on
/// wall-clock PTS, so a static screen (few portal damage frames) still gets
/// a sharp picture instead of `bitrate / configured_fps` scraps.
pub const DEFAULT_BITRATE_KBPS: u32 = 40_000;
pub const DEFAULT_QP: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    Auto,
    Vaapi,
    X264,
    VideoToolbox,
    /// Pure-Rust FilmCraft encoder (cargo feature `filmcraft`), never picked by `Auto`.
    Filmcraft,
}

impl std::str::FromStr for EncoderBackend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "vaapi" => Self::Vaapi,
            "x264" | "software" => Self::X264,
            "videotoolbox" => Self::VideoToolbox,
            "filmcraft" => Self::Filmcraft,
            _ => {
                bail!("unknown encoder backend {s:?} (auto, vaapi, x264, videotoolbox, filmcraft)")
            }
        })
    }
}

impl EncoderBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vaapi => "vaapi",
            Self::X264 => "x264",
            Self::VideoToolbox => "videotoolbox",
            Self::Filmcraft => "filmcraft",
        }
    }

    /// Whether this build can open the backend at all (FilmCraft is a cargo feature).
    pub fn compiled_in(self) -> bool {
        !matches!(self, Self::Filmcraft) || cfg!(feature = "filmcraft")
    }

    /// Backends worth offering on this platform in this build (for the GUI selector).
    pub fn selectable() -> Vec<Self> {
        let mut v = vec![Self::Auto];
        if cfg!(target_os = "linux") {
            v.push(Self::Vaapi);
        }
        if cfg!(target_os = "macos") {
            v.push(Self::VideoToolbox);
        }
        v.push(Self::X264);
        if Self::Filmcraft.compiled_in() {
            v.push(Self::Filmcraft);
        }
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// CBR for VAAPI/VideoToolbox, bitrate mode for x264.
    Auto,
    Cbr,
    Vbr,
    Cqp,
    Icq,
}

impl std::str::FromStr for RateControl {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "cbr" => Self::Cbr,
            "vbr" => Self::Vbr,
            "cqp" | "qp" => Self::Cqp,
            "icq" => Self::Icq,
            _ => bail!("unknown rate control {s:?} (auto, cbr, vbr, cqp, icq)"),
        })
    }
}

impl RateControl {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cbr => "cbr",
            Self::Vbr => "vbr",
            Self::Cqp => "cqp",
            Self::Icq => "icq",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Auto, Self::Cbr, Self::Vbr, Self::Cqp, Self::Icq]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    /// `0` means "use the capture size" (resolved by the pipeline).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub backend: EncoderBackend,
    pub rate_control: RateControl,
    pub qp: u32,
    /// GOP length in frames. `None` = 10 seconds at `fps`.
    pub keyint: Option<u32>,
    pub intra_refresh: bool,
    pub vaapi_device: String,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fps: 60,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            backend: EncoderBackend::Auto,
            rate_control: RateControl::Auto,
            qp: DEFAULT_QP,
            keyint: None,
            intra_refresh: false,
            vaapi_device: "/dev/dri/renderD128".into(),
        }
    }
}

impl EncoderConfig {
    pub fn resolved_size(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        match (self.width, self.height) {
            (0, 0) => (src_w, src_h),
            (0, h) => (src_w, h),
            (w, 0) => (w, src_h),
            (w, h) => (w, h),
        }
    }

    pub fn gop_frames(&self) -> u32 {
        self.keyint
            .filter(|k| *k > 0)
            .unwrap_or_else(|| self.fps.saturating_mul(10).max(1))
    }

    /// Settings that require the encoder to be re-opened (not just an IDR).
    pub fn reopen_needed(&self, other: &Self, out_w: u32, out_h: u32) -> bool {
        let (ew, eh) = self.resolved_size(out_w, out_h);
        ew != out_w
            || eh != out_h
            || self.bitrate_kbps != other.bitrate_kbps
            || self.backend != other.backend
            || self.rate_control != other.rate_control
            || self.qp != other.qp
            || self.keyint != other.keyint
            || self.intra_refresh != other.intra_refresh
            || self.vaapi_device != other.vaapi_device
            || self.fps != other.fps
    }
}

/// Parses `1920x1080` or `1920×1080`.
pub fn parse_size(s: &str) -> Result<(u32, u32)> {
    let s = s.trim().replace('×', "x");
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| anyhow!("size must be WIDTHxHEIGHT, got {s:?}"))?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}
