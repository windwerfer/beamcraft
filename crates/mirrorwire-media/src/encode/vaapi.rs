//! VAAPI helpers: the VPP filter graph (upload, scale, convert to NV12 on the GPU) and the
//! DMA-BUF import (DRM-PRIME frame mapped into a VAAPI surface). Pure move out of `mod.rs`.

use crate::frame::RawFrame;
use crate::hw::HwRef;
use anyhow::{anyhow, bail, Context as _, Result};
use ffmpeg_next::{ffi, format::Pixel};

pub(super) struct VaapiVpp {
    pub(super) graph: ffmpeg_next::filter::Graph,
    pub(super) src_format: Pixel,
    pub(super) src_w: u32,
    pub(super) src_h: u32,
}

// SAFETY: see `Scaler` in `mod.rs`.
unsafe impl Send for VaapiVpp {}

impl VaapiVpp {
    pub(super) fn try_new(device: &HwRef, src: Pixel, w: u32, h: u32) -> Result<Self> {
        Self::build(device, src, w, h, w, h)
    }

    fn build(
        device: &HwRef,
        src: Pixel,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self> {
        Self::build_from_ptr(device.as_ptr(), src, src_w, src_h, dst_w, dst_h)
    }

    pub(super) fn build_from_ptr(
        device: *mut ffi::AVBufferRef,
        src: Pixel,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self> {
        let graph = Self::build_graph(device, VppInput::System(src), src_w, src_h, dst_w, dst_h)?;
        Ok(Self {
            graph,
            src_format: src,
            src_w,
            src_h,
        })
    }

    /// buffer → [hwupload] → scale_vaapi (to NV12, output size) → buffersink.
    /// System-memory input gets an hwupload; VAAPI input (mapped DMA-BUF)
    /// goes straight to VPP.
    pub(super) fn build_graph(
        device: *mut ffi::AVBufferRef,
        input: VppInput,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<ffmpeg_next::filter::Graph> {
        let Some(buffer) = ffmpeg_next::filter::find("buffer") else {
            bail!("libavfilter buffer is not in this FFmpeg");
        };
        let Some(sink) = ffmpeg_next::filter::find("buffersink") else {
            bail!("libavfilter buffersink is not in this FFmpeg");
        };
        let vpp = ffmpeg_next::filter::find("scale_vaapi")
            .or_else(|| ffmpeg_next::filter::find("vpp_vaapi"))
            .ok_or_else(|| anyhow!("scale_vaapi/vpp_vaapi is not in this FFmpeg"))?;
        let vpp_name = vpp.name().to_owned();

        let mut graph = ffmpeg_next::filter::Graph::new();
        let vpp_args = format!("w={dst_w}:h={dst_h}:format=nv12");
        let mut in_ctx = match input {
            VppInput::System(src) => {
                // Pixel formats by number: FFmpeg names (bgr0) differ from
                // the Rust enum names (BGRZ).
                let buf_args = format!(
                    "video_size={src_w}x{src_h}:pix_fmt={}:time_base=1/1000000:pixel_aspect=1/1",
                    ffi::AVPixelFormat::from(src) as i32
                );
                graph.add(&buffer, "in", &buf_args)?
            }
            VppInput::Vaapi(frames) => {
                alloc_vaapi_buffersrc(&mut graph, &buffer, src_w, src_h, frames)?
            }
        };
        let mut vpp_ctx = alloc_hw_filter(&mut graph, &vpp, "vpp", &vpp_args, device)?;
        let mut out_ctx = graph.add(&sink, "out", "")?;
        // Keep the result on a VAAPI surface so we do not download and
        // re-upload through system memory.
        // SAFETY: `out_ctx` is a live filter context; `bytes` views the local `fmts` array
        // (plain `i32`-sized enum values, NONE-terminated) and outlives the call, which copies it.
        unsafe {
            let fmts = [
                ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
                ffi::AVPixelFormat::AV_PIX_FMT_NONE,
            ];
            let bytes = std::slice::from_raw_parts(
                fmts.as_ptr() as *const u8,
                std::mem::size_of_val(&fmts),
            );
            let ret = ffi::av_opt_set_bin(
                out_ctx.as_mut_ptr().cast(),
                c"pix_fmts".as_ptr(),
                bytes.as_ptr(),
                (std::mem::size_of::<ffi::AVPixelFormat>() * (fmts.len() - 1)) as i32,
                ffi::AV_OPT_SEARCH_CHILDREN,
            );
            if ret < 0 {
                tracing::warn!(
                    "could not pin buffersink to VAAPI ({})",
                    ffmpeg_next::Error::from(ret)
                );
            }
        }
        if matches!(input, VppInput::System(_)) {
            let Some(hwupload) = ffmpeg_next::filter::find("hwupload") else {
                bail!("hwupload filter is not in this FFmpeg");
            };
            let mut up_ctx = alloc_hw_filter(&mut graph, &hwupload, "upload", "", device)?;
            in_ctx.link(0, &mut up_ctx, 0);
            up_ctx.link(0, &mut vpp_ctx, 0);
        } else {
            in_ctx.link(0, &mut vpp_ctx, 0);
        }
        vpp_ctx.link(0, &mut out_ctx, 0);
        graph
            .validate()
            .with_context(|| format!("configuring {vpp_name} graph"))?;
        Ok(graph)
    }
}

// `Vaapi` is only built by the unix DMA-BUF path.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Copy)]
pub(super) enum VppInput {
    System(Pixel),
    Vaapi(*mut ffi::AVBufferRef),
}

/// A buffer source that takes VAAPI frames from `frames`.
fn alloc_vaapi_buffersrc(
    graph: &mut ffmpeg_next::filter::Graph,
    buffer: &ffmpeg_next::filter::Filter,
    w: u32,
    h: u32,
    frames: *mut ffi::AVBufferRef,
) -> Result<ffmpeg_next::filter::Context> {
    // SAFETY: `graph` and `buffer` are live; the allocated `ctx` is checked for null and is
    // owned by the graph. `par` is checked for null, filled with plain values, and freed with
    // `av_free` after `av_buffersrc_parameters_set` (which takes its own reference to
    // `frames`, a valid hw frames reference by caller contract).
    unsafe {
        let ctx =
            ffi::avfilter_graph_alloc_filter(graph.as_mut_ptr(), buffer.as_ptr(), c"in".as_ptr());
        if ctx.is_null() {
            bail!("allocating buffer source");
        }
        let par = ffi::av_buffersrc_parameters_alloc();
        if par.is_null() {
            bail!("av_buffersrc_parameters_alloc failed");
        }
        (*par).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
        (*par).width = w as i32;
        (*par).height = h as i32;
        (*par).time_base = ffi::AVRational {
            num: 1,
            den: 1_000_000,
        };
        (*par).sample_aspect_ratio = ffi::AVRational { num: 1, den: 1 };
        (*par).hw_frames_ctx = frames; // av_buffersrc_parameters_set takes its own reference
        let ret = ffi::av_buffersrc_parameters_set(ctx, par);
        ffi::av_free(par.cast());
        if ret < 0 {
            bail!(
                "buffer source parameters: {}",
                ffmpeg_next::Error::from(ret)
            );
        }
        let ret = ffi::avfilter_init_str(ctx, std::ptr::null());
        if ret < 0 {
            bail!("init buffer source: {}", ffmpeg_next::Error::from(ret));
        }
        Ok(ffmpeg_next::filter::Context::wrap(ctx))
    }
}

/// Keeps our duplicate of the DMA-BUF fd alive for as long as FFmpeg holds
/// the DRM-PRIME frame (until the mapped VAAPI surface is released).
#[cfg(unix)]
struct DrmHolder {
    desc: ffi::AVDRMFrameDescriptor,
    _fd: std::os::fd::OwnedFd,
}

/// # Safety
/// FFmpeg calls this once when the buffer is freed, with the `opaque` pointer given to
/// `av_buffer_create` (null or a `Box<DrmHolder>` turned into a raw pointer by `map_dmabuf`).
#[cfg(unix)]
unsafe extern "C" fn free_drm_holder(opaque: *mut std::ffi::c_void, _data: *mut u8) {
    if !opaque.is_null() {
        // SAFETY: per the contract above `opaque` is the unique `Box<DrmHolder>` pointer and
        // this callback runs exactly once, so reclaiming the box is sound.
        drop(unsafe { Box::from_raw(opaque as *mut DrmHolder) });
    }
}

/// Wrap a DMA-BUF as an `AV_PIX_FMT_DRM_PRIME` frame and map it into a VAAPI
/// surface from `frames` (`av_hwframe_map`, no copy). Needs FFmpeg built with
/// libdrm; otherwise this fails with "Function not implemented".
#[cfg(unix)]
pub(super) fn map_dmabuf(
    frames: *mut ffi::AVBufferRef,
    raw: &RawFrame,
    dmabuf: &crate::frame::DmaBufFrame,
) -> Result<ffmpeg_next::frame::Video> {
    use std::os::fd::AsRawFd;
    let fd = dmabuf.fd.try_clone().context("duplicating DMA-BUF fd")?;
    let size = if dmabuf.size > 0 {
        dmabuf.size
    } else {
        (dmabuf.offset as usize)
            .saturating_add((dmabuf.pitch as usize).saturating_mul(raw.height as usize))
    };
    // SAFETY: AVDRMFrameDescriptor is plain old data; all-zero is valid.
    let mut desc: ffi::AVDRMFrameDescriptor = unsafe { std::mem::zeroed() };
    desc.nb_objects = 1;
    desc.objects[0].fd = fd.as_raw_fd();
    desc.objects[0].size = size;
    desc.objects[0].format_modifier = dmabuf.modifier;
    desc.nb_layers = 1;
    desc.layers[0].format = raw.format.drm_fourcc();
    desc.layers[0].nb_planes = 1;
    desc.layers[0].planes[0].object_index = 0;
    desc.layers[0].planes[0].offset = dmabuf.offset as isize;
    desc.layers[0].planes[0].pitch = dmabuf.pitch as isize;

    let holder = Box::into_raw(Box::new(DrmHolder { desc, _fd: fd }));
    // SAFETY: `holder` is a valid, unique heap pointer. `av_buffer_create` hands it to
    // `free_drm_holder`, which frees it exactly once; on failure we free it ourselves. The DRM
    // frame `f` owns `buf`, so the descriptor (and the dup'd fd) lives until FFmpeg drops it,
    // and `frames` is a valid hw frames reference (caller contract).
    unsafe {
        let buf = ffi::av_buffer_create(
            std::ptr::addr_of_mut!((*holder).desc).cast(),
            std::mem::size_of::<ffi::AVDRMFrameDescriptor>(),
            Some(free_drm_holder),
            holder.cast(),
            0,
        );
        if buf.is_null() {
            drop(Box::from_raw(holder));
            bail!("av_buffer_create failed");
        }
        let mut drm = ffmpeg_next::frame::Video::empty();
        let f = drm.as_mut_ptr();
        (*f).format = ffi::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        (*f).width = raw.width as i32;
        (*f).height = raw.height as i32;
        (*f).buf[0] = buf; // the frame now owns the holder
        (*f).data[0] = std::ptr::addr_of_mut!((*holder).desc).cast();

        let mut mapped = ffmpeg_next::frame::Video::empty();
        let m = mapped.as_mut_ptr();
        (*m).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
        (*m).hw_frames_ctx = ffi::av_buffer_ref(frames);
        let ret = ffi::av_hwframe_map(m, f, ffi::AV_HWFRAME_MAP_READ as i32);
        if ret < 0 {
            bail!(
                "VAAPI could not import the DMA-BUF (modifier {:#x}): {}",
                dmabuf.modifier,
                ffmpeg_next::Error::from(ret)
            );
        }
        Ok(mapped)
    }
}

fn alloc_hw_filter(
    graph: &mut ffmpeg_next::filter::Graph,
    filter: &ffmpeg_next::filter::Filter,
    name: &str,
    args: &str,
    device: *mut ffi::AVBufferRef,
) -> Result<ffmpeg_next::filter::Context> {
    use std::ffi::CString;
    let cname = CString::new(name)?;
    let cargs = if args.is_empty() {
        None
    } else {
        Some(CString::new(args)?)
    };
    // SAFETY: `graph`/`filter` are live; `cname`/`cargs` are NUL-terminated and outlive the
    // calls; `ctx` is null-checked and owned by the graph; `device` is a valid buffer
    // reference (caller contract) and `av_buffer_ref` gives the filter its own reference.
    unsafe {
        let ctx =
            ffi::avfilter_graph_alloc_filter(graph.as_mut_ptr(), filter.as_ptr(), cname.as_ptr());
        if ctx.is_null() {
            bail!("allocating {name} filter");
        }
        (*ctx).hw_device_ctx = ffi::av_buffer_ref(device);
        let ret = ffi::avfilter_init_str(
            ctx,
            cargs
                .as_ref()
                .map(|s| s.as_ptr())
                .unwrap_or(std::ptr::null()),
        );
        if ret < 0 {
            bail!("init {name}: {}", ffmpeg_next::Error::from(ret));
        }
        Ok(ffmpeg_next::filter::Context::wrap(ctx))
    }
}
