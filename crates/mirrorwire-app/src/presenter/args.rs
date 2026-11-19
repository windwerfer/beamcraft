//! Command-line arguments of `mirrorwire present` (also the GUI's defaults).

use crate::capture_prefs::{parse_fps_arg, parse_mode_arg, CapturePrefs, CursorPref, FpsPref};
use anyhow::Result;
use clap::{Args, ValueEnum};
use mirrorwire_media::encode::{
    parse_size, EncoderBackend, EncoderConfig, RateControl, DEFAULT_BITRATE_KBPS, DEFAULT_QP,
};
use mirrorwire_media::source::CaptureMode;
use mirrorwire_proto::fec::FecMode;
use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
pub enum SourceKind {
    /// Screen capture through xdg-desktop-portal + PipeWire.
    Portal,
    /// Synthetic moving pattern with an embedded frame counter.
    Test,
}

#[derive(Args)]
pub struct PresentArgs {
    #[arg(long, value_enum, default_value_t = SourceKind::Portal)]
    pub(super) source: SourceKind,
    /// What to capture: mirror (default; an existing screen) or extend (a
    /// virtual monitor the viewer shows as an extra display; needs
    /// compositor support, otherwise mirror is used with a note). Also
    /// MIRRORWIRE_MODE or the saved setting.
    #[arg(long, value_parser = parse_mode_arg, value_name = "mirror|extend")]
    pub mode: Option<CaptureMode>,
    /// Output width. 0 (default) follows the native capture size.
    #[arg(long, default_value_t = 0)]
    pub(super) width: u32,
    /// Output height. 0 (default) follows the native capture size.
    #[arg(long, default_value_t = 0)]
    pub(super) height: u32,
    /// Output size as WIDTHxHEIGHT. Overrides --width/--height. Default: native capture size.
    #[arg(long, value_name = "WxH")]
    pub(super) size: Option<String>,
    /// Highest frame rate to ask the compositor for: 30, 60 (default), another
    /// number, or max (the display's refresh rate). Also MIRRORWIRE_FPS or the
    /// saved setting.
    #[arg(long, value_parser = parse_fps_arg, value_name = "30|60|max")]
    pub fps: Option<FpsPref>,
    /// How the mouse cursor is sent: separate (as coordinates, drawn by the
    /// viewer; default), embedded (drawn into the video) or hidden. Also
    /// MIRRORWIRE_CURSOR or the saved setting. separate falls back to embedded
    /// when the portal or the viewer can't do it.
    #[arg(long, value_enum)]
    pub cursor: Option<CursorPref>,
    /// Forward error correction for video packets: auto (default; parity
    /// adapts to the loss the viewer reports), off, or a fixed percentage of
    /// parity packets such as 20. Also MIRRORWIRE_FEC or the saved setting.
    #[arg(long, value_parser = crate::capture_prefs::parse_fec_arg, value_name = "auto|off|PERCENT")]
    pub fec: Option<FecMode>,
    /// Adaptive bitrate (default on): the bitrate setting is the maximum;
    /// the presenter lowers it when the viewer reports loss or the RTT grows
    /// and raises it again slowly. FEC parity is counted inside it. Also
    /// MIRRORWIRE_ADAPTIVE_BITRATE or the saved setting.
    #[arg(long, value_parser = crate::capture_prefs::parse_on_off_arg, value_name = "on|off")]
    pub adaptive_bitrate: Option<bool>,
    /// Resolved from --fps / --cursor / --fec / env / settings.json in `main`.
    #[arg(skip)]
    pub capture: CapturePrefs,
    /// Target bitrate in kbit/s (default: 40000 = 40 Mbit/s).
    #[arg(long, default_value_t = DEFAULT_BITRATE_KBPS)]
    pub(super) bitrate_kbps: u32,
    /// Target bitrate in Mbit/s; overrides --bitrate-kbps when set.
    #[arg(long)]
    pub(super) bitrate_mbps: Option<f64>,
    /// auto, vaapi, x264, videotoolbox, filmcraft (pure-Rust; only in builds with the
    /// `filmcraft` feature, never chosen by auto).
    #[arg(long, default_value = "auto")]
    pub(super) encoder: EncoderBackend,
    /// Rate control: auto, cbr, vbr, cqp, icq. auto is CBR at the target bitrate.
    #[arg(long, default_value = "auto")]
    pub(super) rc: RateControl,
    /// QP / ICQ quality when --rc cqp or --rc icq (lower is sharper; default 18).
    #[arg(long, default_value_t = DEFAULT_QP)]
    pub(super) qp: u32,
    /// Keyframe interval in frames. 0 (default) is 10 seconds at --fps.
    #[arg(long, default_value_t = 0)]
    pub(super) keyint: u32,
    #[arg(long, default_value = "/dev/dri/renderD128")]
    pub(super) vaapi_device: String,
    /// Spread intra coding over frames instead of sending large keyframes.
    #[arg(long)]
    pub(super) intra_refresh: bool,
    /// Set from the global `--port` / `MIRRORWIRE_PORT` / saved settings in `main`.
    #[arg(skip)]
    pub port: u16,
    /// Address to bind (default: all interfaces).
    #[arg(long)]
    pub(super) bind: Option<IpAddr>,
    /// Address to put in the QR code (default: first non-loopback IPv4).
    #[arg(long)]
    pub(super) advertise_ip: Option<IpAddr>,
    #[arg(long)]
    pub(super) name: Option<String>,
    /// Accept any viewer without a pairing code.
    #[arg(long)]
    pub(super) no_pairing: bool,
    #[arg(long)]
    pub(super) no_mdns: bool,
    /// Write the mirrorwire:// URI to this file (for scripts and tests).
    #[arg(long)]
    pub(super) uri_file: Option<PathBuf>,
    /// Stop after this many frames (test source only).
    #[arg(long)]
    pub(super) frames: Option<u64>,
    /// Hide the mouse cursor in the capture (same as --cursor hidden).
    #[arg(long)]
    pub hide_cursor: bool,
    /// Don't offer DMA-BUF (zero-copy GPU buffers) to the compositor; always
    /// capture through shared memory. For debugging driver problems.
    #[arg(long)]
    pub(super) no_dmabuf: bool,
}

impl PresentArgs {
    pub fn encoder_config(&self) -> Result<EncoderConfig> {
        let (width, height) = if let Some(s) = &self.size {
            parse_size(s)?
        } else {
            (self.width, self.height)
        };
        let bitrate_kbps = if let Some(mbps) = self.bitrate_mbps {
            (mbps * 1000.0).round().max(1.0) as u32
        } else {
            self.bitrate_kbps
        };
        Ok(EncoderConfig {
            width,
            height,
            // Corrected to the negotiated rate once the capture is running.
            fps: self.capture.fps.request().min(60),
            bitrate_kbps,
            backend: self.encoder,
            rate_control: self.rc,
            qp: self.qp,
            keyint: (self.keyint > 0).then_some(self.keyint),
            intra_refresh: self.intra_refresh,
            vaapi_device: self.vaapi_device.clone(),
            max_size: None,
        })
    }

    #[cfg(feature = "gui")]
    pub fn gui_defaults() -> Self {
        Self {
            mode: None,
            source: SourceKind::Portal,
            width: 0,
            height: 0,
            size: None,
            fps: None,
            cursor: None,
            fec: None,
            adaptive_bitrate: None,
            capture: CapturePrefs::default(),
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            bitrate_mbps: None,
            encoder: EncoderBackend::Auto,
            rc: RateControl::Auto,
            qp: DEFAULT_QP,
            keyint: 0,
            vaapi_device: "/dev/dri/renderD128".into(),
            intra_refresh: false,
            port: mirrorwire_proto::DEFAULT_PORT,
            bind: None,
            advertise_ip: None,
            name: None,
            no_pairing: false,
            no_mdns: false,
            uri_file: None,
            frames: None,
            hide_cursor: false,
            no_dmabuf: false,
        }
    }
}
