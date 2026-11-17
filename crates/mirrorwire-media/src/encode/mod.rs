use crate::frame::{EncodedFrame, PixelFormat, RawFrame};
use crate::hw::{self, HwRef};
use anyhow::{anyhow, bail, Context as _, Result};
use bytes::Bytes;
use ffmpeg_next::{codec, encoder, ffi, format::Pixel, software::scaling, Dictionary, Packet};
use mirrorwire_proto::clock::now_us;

mod config;
mod rc;
mod vaapi;
pub use config::*;
use rc::{apply_rate_control, apply_vaapi_opts, describe_rc, effective_rc};
#[cfg(unix)]
use vaapi::map_dmabuf;
use vaapi::{VaapiVpp, VppInput};

enum Upload {
    Software,
    Vaapi {
        _device: HwRef,
        frames: HwRef,
        vpp: Option<VaapiVpp>,
        dma: Option<DmaImport>,
    },
}

/// DMA-BUF import: a VAAPI frames context at the capture size (the DRM-PRIME
/// buffers are mapped into it as VAAPI surfaces) and a VPP graph that scales
/// and converts those surfaces to NV12 on the GPU.
struct DmaImport {
    frames: HwRef,
    key: (PixelFormat, u32, u32),
    graph: ffmpeg_next::filter::Graph,
}

// SAFETY: the VAAPI frames reference and filter graph are owned by `DmaImport` alone and used only
// through `&mut`; FFmpeg objects have no thread affinity and the type is not `Sync`.
unsafe impl Send for DmaImport {}

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

struct Scaler {
    ctx: scaling::Context,
    src: (PixelFormat, u32, u32),
}

// SAFETY: `Scaler` (swscale context) and `VaapiVpp` (filter graph + hw references) own their FFmpeg
// objects exclusively, are used only through `&mut`, and are not `Sync`; FFmpeg has no
// thread affinity.
unsafe impl Send for Scaler {}

/// Low-latency H.264 encoder: no B-frames, no lookahead, one packet out per
/// frame in, IDR on demand. Input is converted to NV12 on the GPU (VAAPI VPP)
/// when that path is available, otherwise on the CPU.
/// The codec doing the actual encoding.
enum Core {
    Ffmpeg(encoder::Video),
    #[cfg(feature = "filmcraft")]
    Filmcraft(Box<crate::filmcraft::FcEncoder>),
}

pub struct Encoder {
    inner: Core,
    upload: Upload,
    scaler: Option<Scaler>,
    staging: ffmpeg_next::frame::Video,
    packed: ffmpeg_next::frame::Video,
    /// The shm frame copied for the VPP graph, kept apart from `packed`
    /// (which then holds the VPP output) so its 8 MB are reused instead of
    /// allocated per frame (2026-11-16: 6.1 ms pack/map, ~49 instead of 60
    /// fps with the 1080p test pattern on h264_vaapi).
    vpp_input: ffmpeg_next::frame::Video,
    config: EncoderConfig,
    name: &'static str,
    conversion: &'static str,
    rc_summary: String,
    start_us: Option<u64>,
    last_pts: Option<i64>,
    /// A converted frame is ready in `packed`/`staging` (for repeats).
    have_frame: bool,
    /// Why the VPP or DMA-BUF path was dropped, for the status line.
    fallback_note: Option<String>,
    last_pack_us: u64,
    last_convert_us: u64,
    last_upload_us: u64,
    last_encode_us: u64,
}

// SAFETY: `Encoder` owns every FFmpeg context, frame and hw reference it holds and uses them only
// through `&mut self`; it is not `Sync`, so no two threads use them at once.
unsafe impl Send for Encoder {}

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
        let mut last_err = anyhow!("no encoder backend tried");
        for &backend in order {
            match Self::open(&config, backend) {
                Ok(enc) => return Ok(enc),
                Err(e) => {
                    tracing::warn!("encoder {backend:?} unavailable: {e:#}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn conversion_path(&self) -> &'static str {
        self.conversion
    }

    pub fn rc_summary(&self) -> &str {
        &self.rc_summary
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// Whether [`Self::set_bitrate_live`] works for this encoder: libx264 in
    /// a bitrate mode re-reads `bit_rate`/`rc_max_rate`/`rc_buffer_size`
    /// before each frame (FFmpeg calls `x264_encoder_reconfig`). VAAPI,
    /// VideoToolbox and FilmCraft only read them when opened.
    pub fn supports_live_bitrate(&self) -> bool {
        matches!(self.inner, Core::Ffmpeg(_))
            && self.name == "libx264"
            && !matches!(
                effective_rc(EncoderBackend::X264, self.config.rate_control),
                RateControl::Cqp | RateControl::Icq
            )
    }

    /// Change the target bitrate for the next frames without reopening the
    /// encoder (no keyframe). Returns `false` (nothing changed) when the
    /// encoder can't do that; the caller reopens it instead.
    pub fn set_bitrate_live(&mut self, kbps: u32) -> bool {
        if !self.supports_live_bitrate() {
            return false;
        }
        let rc = effective_rc(EncoderBackend::X264, self.config.rate_control);
        #[allow(irrefutable_let_patterns)]
        let Core::Ffmpeg(enc) = &mut self.inner
        else {
            return false;
        };
        if apply_rate_control(
            enc,
            EncoderBackend::X264,
            rc,
            kbps as i64 * 1000,
            self.config.qp,
        )
        .is_err()
        {
            return false;
        }
        self.config.bitrate_kbps = kbps;
        let conversion = self.conversion;
        self.rc_summary = describe_rc(enc, self.name, rc, &self.config, conversion);
        true
    }

    pub fn last_timings_ms(&self) -> (f64, f64) {
        (
            self.last_convert_us as f64 / 1000.0,
            self.last_encode_us as f64 / 1000.0,
        )
    }

    /// pack / colour-convert / upload / encode, in milliseconds.
    pub fn last_breakdown_ms(&self) -> (f64, f64, f64, f64) {
        (
            self.last_pack_us as f64 / 1000.0,
            self.last_convert_us as f64 / 1000.0,
            self.last_upload_us as f64 / 1000.0,
            self.last_encode_us as f64 / 1000.0,
        )
    }

    fn open(config: &EncoderConfig, backend: EncoderBackend) -> Result<Self> {
        if backend == EncoderBackend::Filmcraft {
            return Self::open_filmcraft(config);
        }
        let (codec_name, sw_format) = match backend {
            EncoderBackend::Vaapi => ("h264_vaapi", Pixel::NV12),
            EncoderBackend::X264 => ("libx264", Pixel::YUV420P),
            EncoderBackend::VideoToolbox => ("h264_videotoolbox", Pixel::NV12),
            EncoderBackend::Auto | EncoderBackend::Filmcraft => unreachable!(),
        };
        let codec = encoder::find_by_name(codec_name)
            .ok_or_else(|| anyhow!("{codec_name} not built into this FFmpeg"))?;
        let mut enc = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()?;

        let bitrate = config.bitrate_kbps as i64 * 1000;
        let gop = config.gop_frames();
        enc.set_width(config.width);
        enc.set_height(config.height);
        // Microsecond time base so rate control follows wall-clock gaps
        // (portal damage frames can arrive far below the configured fps).
        enc.set_time_base((1, 1_000_000));
        enc.set_frame_rate(Some((config.fps as i32, 1)));
        enc.set_max_b_frames(0);
        enc.set_gop(gop);
        // SAFETY: `raw` points to the live, unopened `AVCodecContext` owned by `enc`, which is
        // borrowed mutably; only plain integer fields are set.
        unsafe {
            let raw = enc.as_mut_ptr();
            (*raw).profile = ffi::FF_PROFILE_H264_HIGH;
            (*raw).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*raw).flags |= ffi::AV_CODEC_FLAG_CLOSED_GOP as i32;
        }

        let rc = effective_rc(backend, config.rate_control);
        apply_rate_control(&mut enc, backend, rc, bitrate, config.qp)?;

        let mut opts = Dictionary::new();
        let mut upload = match backend {
            EncoderBackend::Vaapi => {
                let device = hw::create_device(
                    ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    Some(&config.vaapi_device),
                )?;
                let frames = hw::create_frames(
                    &device,
                    ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
                    ffi::AVPixelFormat::AV_PIX_FMT_NV12,
                    config.width,
                    config.height,
                )?;
                enc.set_format(Pixel::VAAPI);
                // SAFETY: `enc` is a live, unopened context; `new_ref` gives it its own reference
                // to the frames context, which the codec context releases when freed.
                unsafe { (*enc.as_mut_ptr()).hw_frames_ctx = frames.new_ref() };
                apply_vaapi_opts(&mut opts, rc, bitrate, config);
                Upload::Vaapi {
                    vpp: None,
                    dma: None,
                    _device: device,
                    frames,
                }
            }
            EncoderBackend::X264 => {
                enc.set_format(sw_format);
                opts.set("preset", "ultrafast");
                opts.set("tune", "zerolatency");
                opts.set("forced-idr", "1");
                opts.set("profile", "high");
                if config.intra_refresh {
                    opts.set("intra-refresh", "1");
                }
                // Bitrate modes use a VBV cap (apply_rate_control) and no
                // `nal-hrd=cbr`: HRD filler pads every frame to the target
                // (wasted airtime on a congested link), and x264 refuses VBV
                // changes while HRD is on, which the adaptive bitrate needs
                // (set_bitrate_live).
                if rc == RateControl::Cqp {
                    opts.set("crf", &config.qp.to_string());
                }
                Upload::Software
            }
            EncoderBackend::VideoToolbox => {
                enc.set_format(sw_format);
                opts.set("realtime", "1");
                opts.set("allow_sw", "0");
                opts.set("prio_speed", "1");
                opts.set("profile", "high");
                if matches!(rc, RateControl::Cqp) {
                    opts.set("quality", &format_vt_quality(config.qp));
                }
                Upload::Software
            }
            EncoderBackend::Auto | EncoderBackend::Filmcraft => unreachable!(),
        };

        let inner = enc
            .open_as_with(codec, opts)
            .with_context(|| format!("opening {codec_name}"))?;

        let mut conversion = path::SHM_CPU;
        if let Upload::Vaapi {
            _device: ref device,
            ..
        } = upload
        {
            match VaapiVpp::try_new(device, Pixel::BGRA, config.width, config.height) {
                Ok(_) => {
                    // Built for the output size; rebuilt on the first frame if
                    // the capture size differs. Probe succeeded, so the path
                    // is available.
                    conversion = path::SHM_VPP;
                }
                Err(e) => {
                    tracing::info!(
                        "VAAPI VPP (hwupload/scale_vaapi) unavailable ({e:#}); using CPU BGRx→NV12"
                    );
                    conversion = path::SHM_CPU_UPLOAD;
                }
            }
        }

        // Attach a real VPP graph only when the path is enabled; the probe
        // graph above was dropped. The first convert() builds one for the
        // source size.
        if conversion == path::SHM_VPP {
            if let Upload::Vaapi { vpp, _device, .. } = &mut upload {
                *vpp = None; // built lazily for the actual capture size/format
                let _ = _device;
            }
        }

        let rc_summary = describe_rc(&inner, codec_name, rc, config, conversion);
        tracing::info!("{rc_summary}");

        Ok(Self {
            inner: Core::Ffmpeg(inner),
            upload,
            scaler: None,
            staging: ffmpeg_next::frame::Video::new(sw_format, config.width, config.height),
            packed: ffmpeg_next::frame::Video::empty(),
            vpp_input: ffmpeg_next::frame::Video::empty(),
            config: config.clone(),
            name: codec_name,
            conversion,
            rc_summary,
            start_us: None,
            last_pts: None,
            have_frame: false,
            fallback_note: None,
            last_pack_us: 0,
            last_convert_us: 0,
            last_upload_us: 0,
            last_encode_us: 0,
        })
    }

    #[cfg(feature = "filmcraft")]
    fn open_filmcraft(config: &EncoderConfig) -> Result<Self> {
        let fc = crate::filmcraft::FcEncoder::new(config)?;
        let rc_summary = fc.summary.clone();
        tracing::info!("{rc_summary}");
        Ok(Self {
            inner: Core::Filmcraft(Box::new(fc)),
            upload: Upload::Software,
            scaler: None,
            staging: ffmpeg_next::frame::Video::new(Pixel::YUV420P, config.width, config.height),
            packed: ffmpeg_next::frame::Video::empty(),
            vpp_input: ffmpeg_next::frame::Video::empty(),
            config: config.clone(),
            name: crate::filmcraft::NAME,
            conversion: path::SHM_CPU,
            rc_summary,
            start_us: None,
            last_pts: None,
            have_frame: false,
            fallback_note: None,
            last_pack_us: 0,
            last_convert_us: 0,
            last_upload_us: 0,
            last_encode_us: 0,
        })
    }

    #[cfg(not(feature = "filmcraft"))]
    fn open_filmcraft(_config: &EncoderConfig) -> Result<Self> {
        bail!("this build has no FilmCraft encoder (cargo feature `filmcraft`)")
    }

    /// Whether this encoder can take DMA-BUF frames (VAAPI only).
    pub fn accepts_dmabuf(&self) -> bool {
        matches!(self.upload, Upload::Vaapi { .. })
    }

    /// Why a faster path (VPP, DMA-BUF) was dropped, if it was.
    pub fn fallback_note(&self) -> Option<&str> {
        self.fallback_note.as_deref()
    }

    pub fn encode(&mut self, raw: &RawFrame, force_keyframe: bool) -> Result<Vec<EncodedFrame>> {
        if raw.dmabuf.is_some() {
            self.convert_dmabuf(raw)?;
        } else {
            self.convert(raw)?;
        }
        self.have_frame = true;
        self.submit(raw.seq, raw.capture_us, force_keyframe)
    }

    /// Encode the last converted frame again with a new timestamp. Used when
    /// the compositor only sends frames on damage, so the viewer keeps getting
    /// a steady (cheap, mostly skip-block) stream. Returns nothing before the
    /// first real frame.
    pub fn encode_repeat(
        &mut self,
        seq: u64,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        if !self.have_frame {
            return Ok(Vec::new());
        }
        self.last_pack_us = 0;
        self.last_convert_us = 0;
        self.submit(seq, capture_us, force_keyframe)
    }

    fn submit(
        &mut self,
        seq: u64,
        capture_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>> {
        let t_upload0 = now_us();
        let start = *self.start_us.get_or_insert(capture_us);
        let mut pts = capture_us.saturating_sub(start) as i64;
        // Repeats and real frames interleave; keep PTS strictly increasing.
        if let Some(last) = self.last_pts {
            if pts <= last {
                pts = last + 1;
            }
        }
        self.last_pts = Some(pts);
        let kind = if force_keyframe {
            ffmpeg_next::picture::Type::I
        } else {
            ffmpeg_next::picture::Type::None
        };

        let mut out = Vec::with_capacity(1);
        #[cfg(feature = "filmcraft")]
        if let Core::Filmcraft(fc) = &mut self.inner {
            self.last_upload_us = 0;
            let t_enc0 = now_us();
            let aus = fc.encode(&self.staging, pts, force_keyframe)?;
            for (data, keyframe) in aus {
                out.push(EncodedFrame {
                    seq,
                    keyframe,
                    data: Bytes::from(data),
                    capture_us,
                    encoded_us: now_us(),
                });
            }
            self.last_encode_us = now_us().saturating_sub(t_enc0);
            return Ok(out);
        }
        // libx264 does its work inside send_frame, so the encode timer starts
        // there for software; for VAAPI it starts after the upload.
        let mut t_send = None;
        match &self.upload {
            Upload::Software => {
                self.staging.set_pts(Some(pts));
                self.staging.set_kind(kind);
                t_send = Some(now_us());
                ffmpeg_of(&mut self.inner)?.send_frame(&self.staging)?;
                self.last_upload_us = 0;
            }
            Upload::Vaapi { frames, .. } => {
                let frames_ptr = frames.as_ptr();
                if is_vaapi_pixel(self.packed.format()) {
                    // Already a GPU surface (VPP output, from shared memory or DMA-BUF).
                    self.packed.set_pts(Some(pts));
                    self.packed.set_kind(kind);
                    ffmpeg_of(&mut self.inner)?.send_frame(&self.packed)?;
                    self.last_upload_us = 0; // upload (if any) happened inside VPP
                } else {
                    // Prefer the VPP/CPU output in `packed` when it holds NV12
                    // system memory, otherwise the CPU scaler output in `staging`.
                    self.transfer_to_vaapi_ptr(frames_ptr, pts, kind)?;
                    self.last_upload_us = now_us().saturating_sub(t_upload0);
                }
            }
        }
        let t_enc0 = t_send.unwrap_or_else(now_us);

        let mut packet = Packet::empty();
        let inner = ffmpeg_of(&mut self.inner)?;
        while inner.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                out.push(EncodedFrame {
                    seq,
                    keyframe: packet.is_key(),
                    data: Bytes::copy_from_slice(data),
                    capture_us,
                    encoded_us: now_us(),
                });
            }
        }
        self.last_encode_us = now_us().saturating_sub(t_enc0);
        Ok(out)
    }

    /// Map a DMA-BUF into VAAPI and convert it with VPP. Any failure is a
    /// `DmaBufError`, so the caller switches the capture to shared memory.
    fn convert_dmabuf(&mut self, raw: &RawFrame) -> Result<()> {
        match self.try_convert_dmabuf(raw) {
            Ok(()) => {
                self.conversion = path::DMABUF_ZERO_COPY;
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!("DMA-BUF import failed: {msg}");
                if let Upload::Vaapi { dma, .. } = &mut self.upload {
                    *dma = None;
                }
                self.fallback_note = Some(format!("DMA-BUF off: {msg}"));
                Err(DmaBufError(msg).into())
            }
        }
    }

    #[cfg(unix)]
    fn try_convert_dmabuf(&mut self, raw: &RawFrame) -> Result<()> {
        let dmabuf = raw
            .dmabuf
            .as_ref()
            .ok_or_else(|| anyhow!("frame has no DMA-BUF"))?;
        let device = match &self.upload {
            Upload::Vaapi { _device, .. } => _device.as_ptr(),
            Upload::Software => bail!("encoder {} cannot import DMA-BUF (needs VAAPI)", self.name),
        };
        let (out_w, out_h) = (self.config.width, self.config.height);
        let key = (raw.format, raw.width, raw.height);
        let rebuild = match &self.upload {
            Upload::Vaapi { dma: Some(d), .. } => d.key != key,
            _ => true,
        };
        if rebuild {
            let frames = hw::create_frames_sized(
                device,
                ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
                raw.format.to_ffmpeg().into(),
                raw.width,
                raw.height,
                0,
            )
            .context("VAAPI frames context for DMA-BUF import")?;
            let graph = VaapiVpp::build_graph(
                device,
                VppInput::Vaapi(frames.as_ptr()),
                raw.width,
                raw.height,
                out_w,
                out_h,
            )?;
            tracing::info!(
                "DMA-BUF import: {:?} {}x{} modifier {:#x} → VAAPI VPP → nv12 {out_w}x{out_h}",
                raw.format,
                raw.width,
                raw.height,
                dmabuf.modifier
            );
            if let Upload::Vaapi { dma, .. } = &mut self.upload {
                *dma = Some(DmaImport { frames, key, graph });
            }
        }
        let Upload::Vaapi {
            dma: Some(import), ..
        } = &mut self.upload
        else {
            bail!("DMA-BUF import state missing");
        };

        let t_map = now_us();
        let mapped = map_dmabuf(import.frames.as_ptr(), raw, dmabuf)?;
        self.last_pack_us = now_us().saturating_sub(t_map);

        let t_vpp = now_us();
        let mut src = import
            .graph
            .get("in")
            .ok_or_else(|| anyhow!("vpp missing in"))?;
        src.source().add(&mapped)?;
        drop(mapped);
        let mut sink = import
            .graph
            .get("out")
            .ok_or_else(|| anyhow!("vpp missing out"))?;
        let mut out = ffmpeg_next::frame::Video::empty();
        sink.sink().frame(&mut out)?;
        if !is_vaapi_pixel(out.format()) {
            bail!(
                "VPP returned system memory ({})",
                format_pixel(out.format())
            );
        }
        self.last_convert_us = now_us().saturating_sub(t_vpp);
        self.packed = out;
        Ok(())
    }

    #[cfg(not(unix))]
    fn try_convert_dmabuf(&mut self, _raw: &RawFrame) -> Result<()> {
        bail!("DMA-BUF is only supported on Linux")
    }

    fn transfer_to_vaapi_ptr(
        &mut self,
        frames: *mut ffi::AVBufferRef,
        pts: i64,
        kind: ffmpeg_next::picture::Type,
    ) -> Result<()> {
        // SAFETY: `frames` is a valid hw frames context reference (caller contract); `hw_frame`
        // is a freshly allocated empty frame and `src` points to a frame owned by `self` that
        // outlives the calls; FFmpeg fills/copies, and ownership stays with the Rust values.
        unsafe {
            let mut hw_frame = ffmpeg_next::frame::Video::empty();
            let ret = ffi::av_hwframe_get_buffer(frames, hw_frame.as_mut_ptr(), 0);
            if ret < 0 {
                bail!("av_hwframe_get_buffer: {}", ffmpeg_next::Error::from(ret));
            }
            let src = if matches!(
                self.packed.format(),
                Pixel::NV12 | Pixel::YUV420P | Pixel::YUVJ420P
            ) {
                self.packed.as_ptr()
            } else {
                self.staging.as_ptr()
            };
            let ret = ffi::av_hwframe_transfer_data(hw_frame.as_mut_ptr(), src, 0);
            if ret < 0 {
                bail!(
                    "av_hwframe_transfer_data: {}",
                    ffmpeg_next::Error::from(ret)
                );
            }
            hw_frame.set_pts(Some(pts));
            hw_frame.set_kind(kind);
            ffmpeg_of(&mut self.inner)?.send_frame(&hw_frame)?;
        }
        Ok(())
    }

    fn convert(&mut self, raw: &RawFrame) -> Result<()> {
        // After a DMA-BUF fallback the label still says zero-copy; shared
        // memory frames use VPP when the device has it.
        if self.conversion == path::DMABUF_ZERO_COPY {
            self.conversion = if self.vpp_failed() {
                path::SHM_CPU_UPLOAD
            } else {
                path::SHM_VPP
            };
        }
        if self.conversion == path::SHM_VPP {
            match self.convert_vaapi_vpp(raw) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(
                        "VAAPI VPP convert failed ({e:#}); falling back to CPU BGRx→NV12 for this session"
                    );
                    self.fallback_note = Some(format!("VPP off: {e:#}"));
                    self.conversion = path::SHM_CPU_UPLOAD;
                    if let Upload::Vaapi { vpp, .. } = &mut self.upload {
                        *vpp = None;
                    }
                }
            }
        }
        self.convert_cpu(raw)
    }

    fn vpp_failed(&self) -> bool {
        self.fallback_note
            .as_deref()
            .is_some_and(|n| n.starts_with("VPP off"))
            || !matches!(self.upload, Upload::Vaapi { .. })
    }

    fn convert_vaapi_vpp(&mut self, raw: &RawFrame) -> Result<()> {
        let pix = raw.format.to_ffmpeg();
        let (out_w, out_h) = (self.config.width, self.config.height);
        let need_new = match &self.upload {
            Upload::Vaapi { vpp: Some(v), .. } => {
                v.src_format != pix || v.src_w != raw.width || v.src_h != raw.height
            }
            Upload::Vaapi { vpp: None, .. } => true,
            _ => return Err(anyhow!("no VAAPI device for VPP")),
        };
        if need_new {
            let device = match &self.upload {
                Upload::Vaapi { _device, .. } => _device.as_ptr(),
                _ => unreachable!(),
            };
            let built = VaapiVpp::build_from_ptr(device, pix, raw.width, raw.height, out_w, out_h)?;
            if let Upload::Vaapi { vpp, .. } = &mut self.upload {
                *vpp = Some(built);
            }
            tracing::info!(
                "VAAPI VPP: {} {}x{} → nv12 {out_w}x{out_h}",
                format_pixel(pix),
                raw.width,
                raw.height
            );
        }
        let t_pack = now_us();
        fill_packed(&mut self.vpp_input, raw)?;
        self.last_pack_us = now_us().saturating_sub(t_pack);
        let t_vpp = now_us();
        let out = self.run_vpp();
        if out.is_err() {
            // The graph may still hold a reference to the input buffer:
            // never write into it again.
            self.vpp_input = ffmpeg_next::frame::Video::empty();
        }
        let out = out?;
        self.last_convert_us = now_us().saturating_sub(t_vpp);
        if !is_vaapi_pixel(out.format()) {
            tracing::warn!(
                "VAAPI VPP delivered system memory ({}); this is a GPU download, will re-upload",
                format_pixel(out.format())
            );
        }
        self.packed = out;
        Ok(())
    }

    /// `vpp_input` through the VPP graph. When this returns a frame,
    /// hwupload has copied the input to the GPU and dropped its reference,
    /// so `vpp_input` can be filled again.
    fn run_vpp(&mut self) -> Result<ffmpeg_next::frame::Video> {
        let vpp = match &mut self.upload {
            Upload::Vaapi { vpp: Some(v), .. } => v,
            _ => bail!("VPP graph missing"),
        };
        let mut src = vpp
            .graph
            .get("in")
            .ok_or_else(|| anyhow!("vpp missing in"))?;
        src.source().add(&self.vpp_input)?;
        let mut sink = vpp
            .graph
            .get("out")
            .ok_or_else(|| anyhow!("vpp missing out"))?;
        let mut out = ffmpeg_next::frame::Video::empty();
        sink.sink().frame(&mut out)?;
        Ok(out)
    }

    fn convert_cpu(&mut self, raw: &RawFrame) -> Result<()> {
        let t0 = now_us();
        self.last_pack_us = 0;
        if is_vaapi_pixel(self.packed.format()) {
            // Drop a stale GPU surface from an earlier VPP/DMA-BUF frame so
            // `submit` uploads the new CPU output instead.
            self.packed = ffmpeg_next::frame::Video::empty();
        }
        let key = (raw.format, raw.width, raw.height);
        if self.scaler.as_ref().is_none_or(|s| s.src != key) {
            let same = raw.width == self.config.width && raw.height == self.config.height;
            let flags = if same {
                scaling::Flags::FAST_BILINEAR
                    | scaling::Flags::ACCURATE_RND
                    | scaling::Flags::FULL_CHR_H_INT
                    | scaling::Flags::FULL_CHR_H_INP
            } else {
                scaling::Flags::LANCZOS
                    | scaling::Flags::ACCURATE_RND
                    | scaling::Flags::FULL_CHR_H_INT
                    | scaling::Flags::FULL_CHR_H_INP
            };
            let ctx = scaling::Context::get(
                raw.format.to_ffmpeg(),
                raw.width,
                raw.height,
                self.staging.format(),
                self.config.width,
                self.config.height,
                flags,
            )?;
            if !same {
                tracing::info!(
                    "CPU scale {}x{} → {}x{} (LANCZOS); prefer omitting --size to keep the native capture size",
                    raw.width,
                    raw.height,
                    self.config.width,
                    self.config.height
                );
            }
            self.scaler = Some(Scaler { ctx, src: key });
        }
        let needed = raw.stride * raw.height as usize;
        if raw.data.len() < needed {
            bail!("raw frame too small: {} < {needed}", raw.data.len());
        }
        let Some(scaler) = self.scaler.as_mut() else {
            bail!("scaler not initialized");
        };
        let src = [
            raw.data.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        ];
        let src_stride = [raw.stride as i32, 0, 0, 0];
        // SAFETY: `src[0]` points to `raw.data`, checked above to hold at least
        // `stride * height` bytes, and the other planes are null for a packed format; `dst` is
        // the live staging frame whose `data`/`linesize` were allocated for the configured size.
        unsafe {
            let dst = self.staging.as_mut_ptr();
            let ret = ffi::sws_scale(
                scaler.ctx.as_mut_ptr(),
                src.as_ptr(),
                src_stride.as_ptr(),
                0,
                raw.height as i32,
                (*dst).data.as_ptr(),
                (*dst).linesize.as_ptr(),
            );
            if ret <= 0 {
                bail!("sws_scale failed: {ret}");
            }
        }
        self.last_convert_us = now_us().saturating_sub(t0);
        Ok(())
    }
}

fn fill_packed(dst: &mut ffmpeg_next::frame::Video, raw: &RawFrame) -> Result<()> {
    let pix = raw.format.to_ffmpeg();
    if dst.width() != raw.width || dst.height() != raw.height || dst.format() != pix {
        *dst = ffmpeg_next::frame::Video::new(pix, raw.width, raw.height);
    }
    let needed = raw.stride * raw.height as usize;
    if raw.data.len() < needed {
        bail!("raw frame too small: {} < {needed}", raw.data.len());
    }
    let dst_stride = dst.stride(0);
    let height = raw.height as usize;
    let row = raw.width as usize * 4;
    if dst_stride == raw.stride {
        let (Some(d), Some(s)) = (dst.data_mut(0).get_mut(..needed), raw.data.get(..needed)) else {
            bail!("encoder frame buffer smaller than the raw frame");
        };
        d.copy_from_slice(s);
    } else {
        let plane = dst.data_mut(0);
        for y in 0..height {
            let s = y * raw.stride;
            let d = y * dst_stride;
            let (Some(dst_row), Some(src_row)) =
                (plane.get_mut(d..d + row), raw.data.get(s..s + row))
            else {
                bail!("encoder frame buffer smaller than the raw frame");
            };
            dst_row.copy_from_slice(src_row);
        }
    }
    Ok(())
}

/// FFmpeg's own name for a pixel format ("bgr0", not the Rust enum's "BGRZ").
fn format_pixel(p: Pixel) -> String {
    // SAFETY: pure lookup; returns null or a pointer to a static string.
    let name = unsafe { ffi::av_get_pix_fmt_name(p.into()) };
    if name.is_null() {
        return format!("{p:?}").to_ascii_lowercase();
    }
    // SAFETY: `name` is non-null (checked above) and points to a static NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

fn ffmpeg_of(inner: &mut Core) -> Result<&mut encoder::Video> {
    match inner {
        Core::Ffmpeg(e) => Ok(e),
        #[cfg(feature = "filmcraft")]
        Core::Filmcraft(_) => bail!("not an FFmpeg encoder"),
    }
}

fn is_vaapi_pixel(p: Pixel) -> bool {
    matches!(p, Pixel::VAAPI)
}

fn format_vt_quality(qp: u32) -> String {
    // VideoToolbox quality is 0..1; map QP 15..40 to ~0.85..0.4.
    let q = (52.0 - qp as f64) / 52.0;
    format!("{:.3}", q.clamp(0.2, 1.0))
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
    fn ffmpeg_pixel_names_are_ffmpeg_names() {
        // The VPP buffer source once got "bgrz" (Rust enum name), which FFmpeg
        // rejects, so every BGRx capture fell back to the CPU path.
        assert_eq!(format_pixel(Pixel::BGRZ), "bgr0");
        assert_eq!(format_pixel(Pixel::BGRA), "bgra");
        assert_eq!(format_pixel(Pixel::RGBZ), "rgb0");
    }

    /// Noise at 30 fps: x264 spends whatever the rate control allows.
    fn noise_frame(seq: u64, w: u32, h: u32, rng: &mut u64) -> RawFrame {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for px in data.chunks_mut(8) {
            *rng ^= *rng << 13;
            *rng ^= *rng >> 7;
            *rng ^= *rng << 17;
            px.copy_from_slice(&rng.to_le_bytes()[..px.len()]);
        }
        RawFrame {
            seq,
            width: w,
            height: h,
            stride: (w * 4) as usize,
            format: crate::frame::PixelFormat::Bgrx,
            data,
            capture_us: 1_000_000 + seq * 33_333,
            dmabuf: None,
        }
    }

    #[test]
    fn x264_bitrate_changes_live_without_a_keyframe() {
        crate::init().unwrap();
        let cfg = EncoderConfig {
            backend: EncoderBackend::X264,
            width: 320,
            height: 240,
            fps: 30,
            bitrate_kbps: 4000,
            ..EncoderConfig::default()
        };
        let mut enc = Encoder::new(cfg).unwrap();
        assert!(enc.supports_live_bitrate());
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut run = |enc: &mut Encoder, from: u64| {
            let (mut bytes, mut keyframes) = (0usize, 0usize);
            for seq in from..from + 60 {
                for f in enc
                    .encode(&noise_frame(seq, 320, 240, &mut rng), seq == 0)
                    .unwrap()
                {
                    if seq >= from + 30 {
                        bytes += f.data.len();
                    }
                    keyframes += usize::from(f.keyframe && seq > 0);
                }
            }
            (bytes, keyframes)
        };
        let (high, _) = run(&mut enc, 0);
        assert!(enc.set_bitrate_live(1000));
        assert_eq!(enc.config().bitrate_kbps, 1000);
        assert!(enc.rc_summary().contains("1.0"), "{}", enc.rc_summary());
        let (low, keyframes) = run(&mut enc, 60);
        eprintln!("x264 noise 320x240: 4 Mbit/s -> {high} B/s, live 1 Mbit/s -> {low} B/s");
        assert_eq!(keyframes, 0, "no IDR for a bitrate change");
        // 1 s at 4 vs 1 Mbit/s (generous: VBV start-up, frame overhead).
        assert!(
            (low as f64) < high as f64 * 0.5,
            "bitrate did not drop: {high} -> {low} bytes per second"
        );
        // A quality mode can't change live; nothing is touched.
        let mut cqp = Encoder::new(EncoderConfig {
            rate_control: RateControl::Cqp,
            ..enc.config().clone()
        })
        .unwrap();
        assert!(!cqp.set_bitrate_live(500));
    }

    #[test]
    fn default_bitrate_is_forty_mbps() {
        assert_eq!(DEFAULT_BITRATE_KBPS, 40_000);
        assert_eq!(EncoderConfig::default().bitrate_kbps, 40_000);
        assert_eq!(EncoderConfig::default().width, 0);
    }
}
