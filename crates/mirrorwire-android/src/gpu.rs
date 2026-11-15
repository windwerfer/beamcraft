//! Zero-copy video output: MediaCodec renders into an `AImageReader` of
//! `AIMAGE_FORMAT_PRIVATE` buffers, and each picture's `AHardwareBuffer` is
//! shown as a `GL_TEXTURE_EXTERNAL_OES` texture through an EGLImage. The GPU
//! samples the buffer in whatever layout the decoder wrote it (MTK writes a
//! tiled YUV that the CPU path cannot read: "plane 1: CodecErrorReclaimed"),
//! and the YUV → RGB conversion and the RGBA upload disappear.
//!
//! This file holds all of the Android viewer's `unsafe` (AGENTS.md rule 12):
//! the EGL/GLES extension calls have no safe wrapper in glow, egui or ndk, and
//! ndk's `Image`/`ImageReader` are not `Send`, although the NDK objects behind
//! them lock internally. The public API is safe.

use anyhow::{anyhow, bail, Result};
use eframe::glow::{self, HasContext};
use ndk::media::image_reader::{Image, ImageReader};
use std::ffi::{c_char, c_void, CStr};
use std::sync::{Arc, Mutex};

/// An `ImageReader` shared between the decode thread (which acquires images)
/// and the frames it handed out (which keep it alive until they are dropped).
pub struct SharedReader(ImageReader);

// SAFETY: AImageReader guards its state with an internal mutex (AOSP
// NdkImageReader.cpp, `mLock` in acquire/release/delete), so its functions
// may be called from any thread. The UI thread never calls into the reader;
// it only drops the last `Arc`, which runs `AImageReader_delete` once.
unsafe impl Send for SharedReader {}
// SAFETY: see `Send`; `&ImageReader` methods are internally synchronised.
unsafe impl Sync for SharedReader {}

impl SharedReader {
    pub fn new(reader: ImageReader) -> Arc<Self> {
        Arc::new(Self(reader))
    }

    pub fn reader(&self) -> &ImageReader {
        &self.0
    }
}

/// One decoded picture still owned by its image reader. Dropping it returns
/// the buffer to the decoder.
pub struct GpuFrame {
    // Field order matters: the image is deleted before the reader can be.
    image: Image,
    _reader: Arc<SharedReader>,
    pub width: u32,
    pub height: u32,
    /// Visible part of the buffer in texture coordinates (u0, v0, u1, v1).
    uv: [f32; 4],
}

// SAFETY: an AImage is a handle into its reader's buffer queue; AImage_delete
// takes the reader's lock, and the image is only ever used by one thread at a
// time (it is moved from the decode thread to the UI thread). `_reader` keeps
// the reader alive until the image is gone.
unsafe impl Send for GpuFrame {}

impl GpuFrame {
    pub fn new(image: Image, reader: Arc<SharedReader>) -> Result<Self> {
        let buffer = image
            .hardware_buffer()
            .map_err(|e| anyhow!("image hardware buffer: {e:?}"))?;
        let desc = buffer.describe();
        let (bw, bh) = (desc.width.max(1) as f32, desc.height.max(1) as f32);
        let (w, h) = (
            image.width().unwrap_or(desc.width as i32),
            image.height().unwrap_or(desc.height as i32),
        );
        let (left, top, right, bottom) = match image.crop_rect() {
            Ok(c) if c.right > c.left && c.bottom > c.top => {
                (c.left, c.top, c.right.min(w), c.bottom.min(h))
            }
            _ => (0, 0, w, h),
        };
        Ok(Self {
            width: (right - left).max(1) as u32,
            height: (bottom - top).max(1) as u32,
            uv: [
                left as f32 / bw,
                top as f32 / bh,
                right as f32 / bw,
                bottom as f32 / bh,
            ],
            image,
            _reader: reader,
        })
    }
}

// EGL / GLES constants (EGL 1.4, EGL_KHR_image_base, EGL_ANDROID_image_native_buffer,
// GL_OES_EGL_image_external).
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_NONE: i32 = 0x3038;
const EGL_TRUE: i32 = 1;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

type EglDisplay = *mut c_void;
type EglImage = *mut c_void;
type GetNativeClientBuffer = unsafe extern "C" fn(*const c_void) -> *mut c_void;
type CreateImage =
    unsafe extern "C" fn(EglDisplay, *mut c_void, u32, *mut c_void, *const i32) -> EglImage;
type DestroyImage = unsafe extern "C" fn(EglDisplay, EglImage) -> u32;
type ImageTargetTexture = unsafe extern "C" fn(u32, *mut c_void);

#[link(name = "EGL")]
extern "C" {
    fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    fn eglGetCurrentDisplay() -> EglDisplay;
    fn eglQueryString(display: EglDisplay, name: i32) -> *const c_char;
}

/// EGL entry points, looked up once on the UI thread.
#[derive(Clone, Copy)]
struct Egl {
    display: EglDisplay,
    get_client_buffer: GetNativeClientBuffer,
    create_image: CreateImage,
    destroy_image: DestroyImage,
    target_texture: ImageTargetTexture,
}

// SAFETY: plain function pointers and the process-wide EGLDisplay handle,
// only used on the UI thread while egui's GL context is current.
unsafe impl Send for Egl {}

impl Egl {
    /// Must run with a current GL context (eframe's creation context or a
    /// paint callback).
    fn load(gl: &glow::Context) -> Result<Self> {
        // SAFETY: eglGetCurrentDisplay has no preconditions.
        let display = unsafe { eglGetCurrentDisplay() };
        if display.is_null() {
            bail!("no current EGL display");
        }
        // SAFETY: valid display; the returned string is static and NUL-terminated (or null).
        let ext = unsafe { eglQueryString(display, EGL_EXTENSIONS) };
        if ext.is_null() {
            bail!("eglQueryString(EGL_EXTENSIONS) failed");
        }
        // SAFETY: non-null, NUL-terminated, lives as long as the display.
        let ext = unsafe { CStr::from_ptr(ext) }.to_string_lossy();
        for need in [
            "EGL_KHR_image_base",
            "EGL_ANDROID_image_native_buffer",
            "EGL_ANDROID_get_native_client_buffer",
        ] {
            if !ext.split_ascii_whitespace().any(|e| e == need) {
                bail!("{need} missing");
            }
        }
        if !gl
            .supported_extensions()
            .contains("GL_OES_EGL_image_external")
        {
            bail!("GL_OES_EGL_image_external missing");
        }
        let proc = |name: &CStr| -> Result<*mut c_void> {
            // SAFETY: NUL-terminated name; eglGetProcAddress has no other preconditions.
            let p = unsafe { eglGetProcAddress(name.as_ptr()) };
            if p.is_null() {
                bail!("{} not found", name.to_string_lossy());
            }
            Ok(p)
        };
        // SAFETY: each pointer comes from eglGetProcAddress for exactly the
        // named function, whose C signature the type alias spells out.
        unsafe {
            Ok(Self {
                display,
                get_client_buffer: std::mem::transmute::<*mut c_void, GetNativeClientBuffer>(proc(
                    c"eglGetNativeClientBufferANDROID",
                )?),
                create_image: std::mem::transmute::<*mut c_void, CreateImage>(proc(
                    c"eglCreateImageKHR",
                )?),
                destroy_image: std::mem::transmute::<*mut c_void, DestroyImage>(proc(
                    c"eglDestroyImageKHR",
                )?),
                target_texture: std::mem::transmute::<*mut c_void, ImageTargetTexture>(proc(
                    c"glEGLImageTargetTexture2DOES",
                )?),
            })
        }
    }

    /// Wraps the frame's hardware buffer in an EGLImage. EGL holds its own
    /// reference to the buffer until `destroy`.
    fn import(&self, frame: &GpuFrame) -> Result<EglImage> {
        let buffer = frame
            .image
            .hardware_buffer()
            .map_err(|e| anyhow!("image hardware buffer: {e:?}"))?;
        let attribs = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
        // SAFETY: `buffer` is a live AHardwareBuffer (the image is acquired and
        // outlives this call); the display is current; attribs is EGL_NONE-terminated.
        let image = unsafe {
            let client = (self.get_client_buffer)(buffer.as_ptr() as *const c_void);
            if client.is_null() {
                bail!("eglGetNativeClientBufferANDROID failed");
            }
            (self.create_image)(
                self.display,
                std::ptr::null_mut(),
                EGL_NATIVE_BUFFER_ANDROID,
                client,
                attribs.as_ptr(),
            )
        };
        if image.is_null() {
            bail!(
                "eglCreateImageKHR failed for a {}x{} decoder buffer",
                frame.width,
                frame.height
            );
        }
        Ok(image)
    }

    fn destroy(&self, image: EglImage) {
        // SAFETY: `image` came from `import` on this display and is destroyed once.
        unsafe { (self.destroy_image)(self.display, image) };
    }

    /// Points the bound GL_TEXTURE_EXTERNAL_OES texture at `image`.
    fn bind(&self, image: EglImage) {
        // SAFETY: a texture is bound to GL_TEXTURE_EXTERNAL_OES and `image` is live.
        unsafe { (self.target_texture)(GL_TEXTURE_EXTERNAL_OES, image) };
    }
}

const VERTEX: &str = "attribute vec2 a_corner;
uniform vec4 u_uv;
varying vec2 v_uv;
void main() {
    v_uv = vec2(mix(u_uv.x, u_uv.z, a_corner.x), mix(u_uv.w, u_uv.y, a_corner.y));
    gl_Position = vec4(a_corner * 2.0 - 1.0, 0.0, 1.0);
}";

const FRAGMENT: &str = "#extension GL_OES_EGL_image_external : require
precision mediump float;
varying vec2 v_uv;
uniform samplerExternalOES u_tex;
void main() {
    gl_FragColor = vec4(texture2D(u_tex, v_uv).rgb, 1.0);
}";

/// The GL side: one external texture, a quad, and the frame on screen.
pub struct VideoRenderer {
    egl: Egl,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    texture: glow::Texture,
    u_uv: Option<glow::UniformLocation>,
    u_tex: Option<glow::UniformLocation>,
    /// Newest frame from the decoder, not yet imported.
    pending: Option<GpuFrame>,
    /// Frame on screen and its EGLImage; kept until the next one replaces it.
    current: Option<(GpuFrame, EglImage)>,
    /// Last import failure, shown in the HUD.
    pub error: Option<String>,
}

// SAFETY: glow handles are plain integers, `Egl` is Send (see above), and
// `GpuFrame` is Send; the EGLImage pointer is only used on the UI thread,
// behind the `Mutex` that wraps this struct.
unsafe impl Send for VideoRenderer {}

pub type SharedRenderer = Arc<Mutex<VideoRenderer>>;

impl VideoRenderer {
    /// Checks the EGL/GL extensions and builds the shader; `Err` means the
    /// viewer uses the CPU path (YUV → RGBA) instead.
    pub fn new(gl: &glow::Context) -> Result<SharedRenderer> {
        let egl = Egl::load(gl)?;
        // SAFETY (glow calls): glow's methods are unsafe because they talk to
        // the current GL context, which eframe made current for this call.
        unsafe {
            let program = gl
                .create_program()
                .map_err(|e| anyhow!("create program: {e}"))?;
            for (kind, src) in [
                (glow::VERTEX_SHADER, VERTEX),
                (glow::FRAGMENT_SHADER, FRAGMENT),
            ] {
                let shader = gl
                    .create_shader(kind)
                    .map_err(|e| anyhow!("create shader: {e}"))?;
                gl.shader_source(shader, src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    bail!("video shader: {log}");
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.bind_attrib_location(program, 0, "a_corner");
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                bail!("video program: {log}");
            }
            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow!("vertex array: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| anyhow!("buffer: {e}"))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let corners: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            let bytes: Vec<u8> = corners.iter().flat_map(|f| f.to_ne_bytes()).collect();
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            let texture = gl.create_texture().map_err(|e| anyhow!("texture: {e}"))?;
            gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(texture));
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_EXTERNAL_OES,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, None);
            let u_uv = gl.get_uniform_location(program, "u_uv");
            let u_tex = gl.get_uniform_location(program, "u_tex");
            Ok(Arc::new(Mutex::new(Self {
                egl,
                program,
                vao,
                vbo,
                texture,
                u_uv,
                u_tex,
                pending: None,
                current: None,
                error: None,
            })))
        }
    }

    /// Queues the decoder's newest picture; an older unshown one goes back
    /// to the decoder right away.
    pub fn push(&mut self, frame: GpuFrame) {
        self.pending = Some(frame);
    }

    /// Drops the frames (disconnect); the GL objects stay for the next session.
    pub fn clear(&mut self) {
        self.pending = None;
        if let Some((_, image)) = self.current.take() {
            self.egl.destroy(image);
        }
        self.error = None;
    }

    /// Draws the current picture into the callback's viewport. Called from an
    /// egui paint callback, so egui's GL context is current.
    pub fn paint(&mut self, gl: &glow::Context) {
        if let Some(frame) = self.pending.take() {
            match self.egl.import(&frame) {
                Ok(image) => {
                    // SAFETY: GL context is current (paint callback).
                    unsafe { gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.texture)) };
                    self.egl.bind(image);
                    if let Some((_, old)) = self.current.replace((frame, image)) {
                        self.egl.destroy(old);
                    }
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("GPU video import: {e:#}")),
            }
        }
        let Some((frame, _)) = &self.current else {
            return;
        };
        let uv = frame.uv;
        // SAFETY: GL context is current (paint callback); all handles were
        // created on this context. egui_glow restores its own state afterwards.
        unsafe {
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.program));
            gl.uniform_4_f32(self.u_uv.as_ref(), uv[0], uv[1], uv[2], uv[3]);
            gl.uniform_1_i32(self.u_tex.as_ref(), 0);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(self.texture));
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, None);
            gl.enable(glow::BLEND);
        }
    }

    pub fn destroy(&mut self, gl: &glow::Context) {
        self.clear();
        // SAFETY: GL context is current (eframe on_exit); handles belong to it.
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_texture(self.texture);
        }
    }
}
