use crate::hw::{self, HwRef};
use anyhow::{anyhow, bail, Result};
use ffmpeg_next::{codec, decoder, ffi, format::Pixel, frame, software::scaling, Packet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwDecode {
    None,
    Auto,
}

/// Low-delay H.264 decoder. Each call takes one complete access unit and returns
/// the decoded picture immediately (no frame reordering or frame threading).
pub struct Decoder {
    inner: decoder::Video,
    _hw: Option<HwRef>,
    hw_frame: frame::Video,
    scratch: frame::Video,
    sw_frame: frame::Video,
    name: String,
}

unsafe impl Send for Decoder {}

/// Which software H.264 decoder runs when there is no hardware decoder (or it is off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareDecoder {
    /// FilmCraft's pure-Rust decoder (`craft-video`).
    FilmCraft,
    /// FFmpeg's libavcodec.
    Ffmpeg,
}

impl SoftwareDecoder {
    /// FilmCraft everywhere except macOS, which keeps FFmpeg for now (decision log 2026-11-15).
    pub fn platform_default() -> Self {
        if cfg!(target_os = "macos") {
            Self::Ffmpeg
        } else {
            Self::FilmCraft
        }
    }
}

/// An H.264 decoder that hands out pictures as [`YuvImage`]: the FFmpeg [`Decoder`]
/// (hardware or software) or FilmCraft's [`FilmDecoder`](crate::film_decode::FilmDecoder).
pub trait VideoDecoder: Send {
    /// Decodes one complete access unit; `None` when no picture came out.
    fn decode_yuv(&mut self, au: &[u8]) -> Result<Option<YuvImage>>;
    /// For the overlay, e.g. `h264 (vaapi)`.
    fn name(&self) -> &str;
    fn is_hardware(&self) -> bool;
}

impl VideoDecoder for Decoder {
    fn decode_yuv(&mut self, au: &[u8]) -> Result<Option<YuvImage>> {
        self.decode(au)?.map(YuvImage::from_frame).transpose()
    }
    fn name(&self) -> &str {
        Decoder::name(self)
    }
    fn is_hardware(&self) -> bool {
        Decoder::is_hardware(self)
    }
}

impl VideoDecoder for crate::film_decode::FilmDecoder {
    fn decode_yuv(&mut self, au: &[u8]) -> Result<Option<YuvImage>> {
        self.decode(au)
    }
    fn name(&self) -> &str {
        crate::film_decode::FilmDecoder::name(self)
    }
    fn is_hardware(&self) -> bool {
        false
    }
}

/// The viewer's decoder: hardware first when `hw` is `Auto`, else (or when it is missing)
/// the chosen software decoder, with the reason in its name.
pub fn open_decoder(hw: HwDecode, software: SoftwareDecoder) -> Result<Box<dyn VideoDecoder>> {
    if hw == HwDecode::Auto {
        match open_hw_device() {
            Ok((dev, label)) => match Decoder::open(Some((dev, label))) {
                Ok(d) => return Ok(Box::new(d)),
                Err(e) => {
                    tracing::warn!("{label} H.264 decode failed to open ({e:#}); falling back to software");
                    return open_software(software, &format!("{label} could not open the H.264 decoder"));
                }
            },
            Err(e) => {
                let note = software_reason();
                tracing::info!("{note} ({e:#}); using the software decoder");
                return open_software(software, note);
            }
        }
    }
    open_software(software, "hardware decode is off")
}

/// The software decoder after the hardware one failed while decoding.
pub fn software_after_hw_failure(software: SoftwareDecoder) -> Result<Box<dyn VideoDecoder>> {
    open_software(software, software_reason())
}

fn open_software(software: SoftwareDecoder, why: &str) -> Result<Box<dyn VideoDecoder>> {
    Ok(match software {
        SoftwareDecoder::FilmCraft => Box::new(crate::film_decode::FilmDecoder::new(why)),
        SoftwareDecoder::Ffmpeg => {
            let mut d = Decoder::open(None)?;
            d.name = format!("h264 (software: {why})");
            Box::new(d)
        }
    })
}

impl Decoder {
    pub fn new(hw: HwDecode) -> Result<Self> {
        match hw {
            HwDecode::None => Self::open(None),
            HwDecode::Auto => match open_hw_device() {
                Ok((dev, label)) => match Self::open(Some((dev, label))) {
                    Ok(decoder) => Ok(decoder),
                    Err(e) => {
                        tracing::warn!(
                            "{label} H.264 decode failed to open ({e:#}); falling back to software"
                        );
                        let mut d = Self::open(None)?;
                        d.name = format!("h264 (software: {label} could not open the H.264 decoder)");
                        Ok(d)
                    }
                },
                Err(e) => {
                    let note = software_reason();
                    tracing::info!("{note} ({e:#}); using the software decoder");
                    let mut d = Self::open(None)?;
                    d.name = format!("h264 (software: {note})");
                    Ok(d)
                }
            },
        }
    }

    fn open(hw: Option<(HwRef, &'static str)>) -> Result<Self> {
        let codec = decoder::find(codec::Id::H264).ok_or_else(|| anyhow!("no H.264 decoder"))?;
        let mut ctx = codec::context::Context::new_with_codec(codec);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 16);
        ctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Slice,
            count: threads,
        });
        #[allow(clippy::unnecessary_cast)]
        unsafe {
            let raw = ctx.as_mut_ptr();
            (*raw).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*raw).flags2 |= ffi::AV_CODEC_FLAG2_FAST as i32;
            (*raw).thread_count = threads as i32;
            (*raw).thread_type = ffi::FF_THREAD_SLICE as i32;
            // Skip loop filter on non-reference pictures: cheaper decode,
            // little impact on a no-B-frame, low-delay stream.
            (*raw).skip_loop_filter = ffi::AVDiscard::AVDISCARD_NONREF;
            if hw.is_some() {
                (*raw).get_format = Some(select_hw_pixel_format);
            }
        }
        tracing::info!("H.264 decoder threads={threads} type=slice low_delay,fast skip_loop_filter=nonref");

        let (name, device) = if let Some((dev, label)) = hw {
            unsafe { (*ctx.as_mut_ptr()).hw_device_ctx = dev.new_ref() };
            (format!("h264 ({label})"), Some(dev))
        } else {
            ("h264 (software)".to_owned(), None)
        };

        let inner = ctx.decoder().video()?;
        Ok(Self {
            inner,
            _hw: device,
            hw_frame: frame::Video::empty(),
            scratch: frame::Video::empty(),
            sw_frame: frame::Video::empty(),
            name,
        })
    }

    /// Software decoder used after the hardware one failed while decoding.
    pub fn software_after_hw_failure() -> Result<Self> {
        let mut d = Self::open(None)?;
        d.name = format!("h264 (software: {})", software_reason());
        Ok(d)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_hardware(&self) -> bool {
        self._hw.is_some()
    }

    /// Decodes one access unit. Returns a CPU-side frame (YUV420P or NV12).
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<&frame::Video>> {
        let packet = Packet::copy(data);
        self.inner.send_packet(&packet)?;
        let mut got = false;
        while self.inner.receive_frame(&mut self.scratch).is_ok() {
            std::mem::swap(&mut self.scratch, &mut self.hw_frame);
            got = true;
        }
        if !got {
            return Ok(None);
        }
        if is_hw_format(self.hw_frame.format()) {
            unsafe {
                let ret = ffi::av_hwframe_transfer_data(
                    self.sw_frame.as_mut_ptr(),
                    self.hw_frame.as_ptr(),
                    0,
                );
                if ret < 0 {
                    bail!("hw frame download: {}", ffmpeg_next::Error::from(ret));
                }
            }
            Ok(Some(&self.sw_frame))
        } else {
            if self._hw.is_some() && !self.name.contains("software") {
                tracing::warn!(
                    "{} did not produce a hardware frame (format {:?}); using software pixels",
                    self.name,
                    self.hw_frame.format()
                );
                self.name = format!("h264 (software: {})", software_reason());
            }
            Ok(Some(&self.hw_frame))
        }
    }
}

/// Why hardware decode is not there, in words for the viewer overlay.
fn software_reason() -> &'static str {
    if cfg!(target_os = "macos") {
        // Pre-2012 Macs (e.g. the iMac 2011's Radeon HD 6770M, also under
        // OpenCore Legacy Patcher) have no VideoToolbox H.264 decoder.
        // Software decode of a 1080p desktop stream takes a few ms there.
        "this Mac's GPU has no VideoToolbox H.264 decoder (normal for pre-2012 Macs); software decode is fine"
    } else {
        "no VAAPI H.264 decoder on this machine"
    }
}

fn is_hw_format(p: Pixel) -> bool {
    matches!(p, Pixel::VAAPI | Pixel::VIDEOTOOLBOX)
}

/// Prefer a VAAPI/VideoToolbox pixel format when the codec offers one, so
/// attaching `hw_device_ctx` actually uses the hardware decoder. Without this
/// callback FFmpeg often stays on YUV420P even when a device was set.
unsafe extern "C" fn select_hw_pixel_format(
    _ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut i = 0isize;
    let mut first = ffi::AVPixelFormat::AV_PIX_FMT_NONE;
    loop {
        let p = *pix_fmts.offset(i);
        if p == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            break;
        }
        if first == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            first = p;
        }
        if p == ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX
            || p == ffi::AVPixelFormat::AV_PIX_FMT_VAAPI
        {
            return p;
        }
        i += 1;
    }
    first
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvLayout {
    Yuv420,
    Nv12,
}

/// Tightly packed YUV planes copied from a decoded frame, for a GL shader.
#[derive(Debug, Clone)]
pub struct YuvImage {
    pub width: u32,
    pub height: u32,
    pub layout: YuvLayout,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub u_stride: usize,
    pub v_stride: usize,
}

impl YuvImage {
    pub fn from_frame(src: &frame::Video) -> Result<Self> {
        let width = src.width();
        let height = src.height();
        let cw = width.div_ceil(2) as usize;
        let ch = height.div_ceil(2) as usize;
        let (layout, y, y_stride, u, u_stride, v, v_stride) = match src.format() {
            Pixel::YUV420P | Pixel::YUVJ420P => {
                let (y, ys) = copy_plane(src, 0, width as usize, height as usize)?;
                let (u, us) = copy_plane(src, 1, cw, ch)?;
                let (v, vs) = copy_plane(src, 2, cw, ch)?;
                (YuvLayout::Yuv420, y, ys, u, us, v, vs)
            }
            Pixel::NV12 | Pixel::NV21 => {
                let (y, ys) = copy_plane(src, 0, width as usize, height as usize)?;
                let (uv, uvs) = copy_plane(src, 1, cw * 2, ch)?;
                (YuvLayout::Nv12, y, ys, uv, uvs, Vec::new(), 0)
            }
            other => bail!("unsupported decoded format {other:?} for YUV display"),
        };
        Ok(Self {
            width,
            height,
            layout,
            y,
            u,
            v,
            y_stride,
            u_stride,
            v_stride,
        })
    }
}

fn copy_plane(src: &frame::Video, plane: usize, width: usize, height: usize) -> Result<(Vec<u8>, usize)> {
    let stride = src.stride(plane);
    if stride == 0 || width == 0 {
        bail!("decoded plane {plane} is empty");
    }
    let data = src.data(plane);
    let need = stride.saturating_mul(height.saturating_sub(1)).saturating_add(width);
    if data.len() < need {
        bail!("decoded plane {plane} is short");
    }
    let mut out = vec![0u8; width * height];
    for y in 0..height {
        let s = y * stride;
        let d = y * width;
        out[d..d + width].copy_from_slice(&data[s..s + width]);
    }
    Ok((out, width))
}

fn open_hw_device() -> Result<(HwRef, &'static str)> {
    if cfg!(target_os = "macos") {
        Ok((
            hw::create_device(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX, None)?,
            "videotoolbox",
        ))
    } else {
        Ok((
            hw::create_device(ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI, None)?,
            "vaapi",
        ))
    }
}

/// Converts decoded frames to tightly packed RGBA for display.
pub struct RgbaConverter {
    ctx: Option<(scaling::Context, Pixel, u32, u32)>,
    out: frame::Video,
}

unsafe impl Send for RgbaConverter {}

impl Default for RgbaConverter {
    fn default() -> Self {
        Self {
            ctx: None,
            out: frame::Video::empty(),
        }
    }
}

impl RgbaConverter {
    /// [`RgbaConverter::convert`] for a [`YuvImage`] (copies its planes into an FFmpeg frame).
    pub fn convert_yuv(&mut self, img: &YuvImage, dst: &mut Vec<u8>) -> Result<(u32, u32)> {
        let (w, h) = (img.width as usize, img.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut frame = match img.layout {
            YuvLayout::Yuv420 => frame::Video::new(Pixel::YUV420P, img.width, img.height),
            YuvLayout::Nv12 => frame::Video::new(Pixel::NV12, img.width, img.height),
        };
        let planes: Vec<(&[u8], usize, usize, usize)> = match img.layout {
            YuvLayout::Yuv420 => vec![
                (&img.y, img.y_stride, w, h),
                (&img.u, img.u_stride, cw, ch),
                (&img.v, img.v_stride, cw, ch),
            ],
            YuvLayout::Nv12 => vec![(&img.y, img.y_stride, w, h), (&img.u, img.u_stride, cw * 2, ch)],
        };
        for (i, (src, stride, width, rows)) in planes.into_iter().enumerate() {
            let dst_stride = frame.stride(i);
            let data = frame.data_mut(i);
            for r in 0..rows {
                let (Some(s), Some(d)) = (
                    src.get(r * stride..r * stride + width),
                    data.get_mut(r * dst_stride..r * dst_stride + width),
                ) else {
                    bail!("YUV plane {i} is short");
                };
                d.copy_from_slice(s);
            }
        }
        self.convert(&frame, dst)
    }

    pub fn convert(&mut self, src: &frame::Video, dst: &mut Vec<u8>) -> Result<(u32, u32)> {
        let key = (src.format(), src.width(), src.height());
        if self
            .ctx
            .as_ref()
            .is_none_or(|(_, f, w, h)| (*f, *w, *h) != key)
        {
            // Same-size YUV→RGBA: bicubic + accurate chroma siting, so 4:2:0
            // desktop text is less muddy. The texture stays at the stream's
            // native width×height; this is not a scale.
            let mut ctx = scaling::Context::get(
                key.0,
                key.1,
                key.2,
                Pixel::RGBA,
                key.1,
                key.2,
                scaling::Flags::BICUBIC
                    | scaling::Flags::ACCURATE_RND
                    | scaling::Flags::FULL_CHR_H_INT
                    | scaling::Flags::FULL_CHR_H_INP,
            )?;
            let threads = std::thread::available_parallelism()
                .map(|n| n.get() as i64)
                .unwrap_or(4)
                .clamp(1, 8);
            unsafe {
                let _ = ffi::av_opt_set_int(
                    ctx.as_mut_ptr().cast(),
                    c"threads".as_ptr(),
                    threads,
                    0,
                );
            }
            self.ctx = Some((ctx, key.0, key.1, key.2));
            self.out = frame::Video::new(Pixel::RGBA, key.1, key.2);
        }
        let (ctx, ..) = self.ctx.as_mut().expect("scaler initialized");
        ctx.run(src, &mut self.out)?;
        let (w, h) = (key.1 as usize, key.2 as usize);
        let stride = self.out.stride(0);
        let data = self.out.data(0);
        dst.clear();
        dst.reserve(w * h * 4);
        for y in 0..h {
            dst.extend_from_slice(&data[y * stride..y * stride + w * 4]);
        }
        Ok((key.1, key.2))
    }
}

/// Returns the luma plane of a decoded frame as (data, stride, width, height).
pub fn luma(frame: &frame::Video) -> (&[u8], usize, usize, usize) {
    (
        frame.data(0),
        frame.stride(0),
        frame.width() as usize,
        frame.height() as usize,
    )
}
