//! The video screen of the viewer window (moved out of `gui/mod.rs`).

use super::*;

impl MirrorApp {
    pub(super) fn video_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(handle) = &self.watching else { return };
        let mut latest = None;
        while let Ok(f) = handle.frames.try_recv() {
            latest = Some(f);
        }
        if let Some(f) = latest {
            self.display_latency_ms = mirrorwire_net::transport::ms(
                mirrorwire_proto::clock::now_us(),
                f.capture_us_viewer,
            );
            match f.picture {
                Picture::Rgba(rgba) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &rgba,
                    );
                    let filter = if self.pixel_exact {
                        egui::TextureOptions::NEAREST
                    } else {
                        egui::TextureOptions::LINEAR
                    };
                    match &mut self.texture {
                        Some(tex) => tex.set(image, filter),
                        None => self.texture = Some(ctx.load_texture("video", image, filter)),
                    }
                    self.yuv = None;
                }
                Picture::Yuv(img) => {
                    self.yuv = Some(Arc::new(img));
                    self.texture = None;
                }
            }
        }
        let status = handle.status.lock().expect("status lock").clone();

        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
        let ppp = ui.ctx().pixels_per_point();
        let mut scale_info = String::new();
        let (tex_w, tex_h) = if let Some(tex) = &self.texture {
            (tex.size()[0] as f32, tex.size()[1] as f32)
        } else if let Some(yuv) = &self.yuv {
            (yuv.width as f32, yuv.height as f32)
        } else {
            (0.0, 0.0)
        };
        if tex_w > 0.0 && tex_h > 0.0 {
            let native_pts = egui::vec2(tex_w / ppp, tex_h / ppp);
            let draw_size = if self.pixel_exact {
                native_pts
            } else {
                let fit = (rect.width() / native_pts.x).min(rect.height() / native_pts.y);
                native_pts * fit
            };
            let draw = egui::Rect::from_center_size(rect.center(), draw_size);
            if let Some(yuv) = &self.yuv {
                ui.painter().add(
                    YuvPaint::new(yuv.clone(), self.yuv_gpu.clone(), self.pixel_exact)
                        .callback(draw),
                );
            } else if let Some(tex) = &self.texture {
                ui.painter().image(
                    tex.id(),
                    draw,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            if status.cursor_mode == Some(CursorMode::Separate) {
                if let Some(cursor) = &status.cursor {
                    let current = cursor.lock().expect("cursor lock").current();
                    if let Some((p, img)) = current {
                        self.draw_cursor(ui, &p, &img, draw);
                    }
                }
            }
            let drawn_px = draw_size * ppp;
            let sx = drawn_px.x / tex_w;
            let sy = drawn_px.y / tex_h;
            let convert = if self.yuv.is_some() {
                "GL YUV shader"
            } else {
                "CPU RGBA"
            };
            scale_info = format!(
                "texture {tex_w:.0}x{tex_h:.0}  window {:.0}x{:.0} px  ppp {ppp:.2}  scale {sx:.2}x/{sy:.2}x  {}  {convert}",
                rect.width() * ppp,
                rect.height() * ppp,
                if self.pixel_exact { "1:1 nearest" } else { "fit linear" },
            );
        }
        let updates = status
            .cursor
            .as_ref()
            .map(|c| c.lock().expect("cursor lock").updates)
            .unwrap_or(0);
        let elapsed = self.cursor_rate.0.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.cursor_rate = (
                Instant::now(),
                updates,
                updates.saturating_sub(self.cursor_rate.1) as f64 / elapsed,
            );
        }
        let cursor_line = match status.cursor_mode {
            Some(CursorMode::Separate) => {
                format!("cursor separate ({:.0} updates/s)", self.cursor_rate.2)
            }
            Some(CursorMode::Embedded) => "cursor embedded (in video)".to_owned(),
            Some(CursorMode::Hidden) => "cursor hidden".to_owned(),
            None => "cursor ?".to_owned(),
        };
        if self.show_stats {
            let stream = status.stream.as_ref();
            let text = format!(
                "{}x{}  source {:.1} fps  {:.2} Mbit/s  {} ({})  {}\n{}  decode {:.2} ms  {}  capture->texture {:.1} ms\nwindow: {}\n{}\n{}\n{}\nS: stats  1: 1:1 pixels  F / double-click: fullscreen  Esc: disconnect",
                status.native_width.max(stream.map(|s| s.width).unwrap_or(0)),
                status.native_height.max(stream.map(|s| s.height).unwrap_or(0)),
                status.source_fps,
                status.bitrate_mbps,
                stream.map(|s| s.codec.as_str()).unwrap_or("h264"),
                stream.map(|s| s.encoder.as_str()).unwrap_or("?"),
                cursor_line,
                status.decoder,
                status.decode_ms,
                self.renderer.label(),
                self.display_latency_ms,
                self.display_server.as_deref().unwrap_or("?"),
                status.last_second,
                status.link.line(),
                scale_info,
            );
            ui.painter().text(
                rect.left_top() + egui::vec2(8.0, 8.0),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(0, 255, 120),
            );
        }

        let response = ui.interact(rect, ui.scope_id().with("video"), egui::Sense::click());
        // Never call Response methods inside `ctx.input(..)`: on a click,
        // `double_clicked()` takes the context lock again, and a nested read
        // lock deadlocks as soon as the decode thread is waiting to write
        // (`request_repaint`). That froze the iMac viewer on a click.
        let double_clicked = response.double_clicked();
        let (toggle_fs, toggle_stats, toggle_1to1, leave) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F) || double_clicked,
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Num1),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if toggle_stats {
            self.show_stats = !self.show_stats;
        }
        if toggle_1to1 {
            self.pixel_exact = !self.pixel_exact;
        }
        if toggle_fs {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        let failed = status.error.clone();
        if leave || failed.is_some() && self.has_picture() && !status.connected {
            self.error = failed;
            self.disconnect(&ctx);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }

    /// The presenter's cursor as a textured quad over the video, mapped
    /// through the same rectangle the video is drawn in.
    fn draw_cursor(
        &mut self,
        ui: &egui::Ui,
        p: &mirrorwire_proto::cursor::PointerUpdate,
        img: &mirrorwire_proto::cursor::CursorImage,
        video: egui::Rect,
    ) {
        let rect = mirrorwire_proto::cursor::RectF {
            x: video.min.x,
            y: video.min.y,
            w: video.width(),
            h: video.height(),
        };
        let Some(r) = mirrorwire_proto::cursor::cursor_rect(p, img, rect) else {
            return;
        };
        if self.cursor_tex.as_ref().is_none_or(|(id, _)| *id != img.id) {
            let image = egui::ColorImage::from_rgba_premultiplied(
                [img.width as usize, img.height as usize],
                &img.rgba,
            );
            let tex = ui
                .ctx()
                .load_texture("cursor", image, egui::TextureOptions::LINEAR);
            self.cursor_tex = Some((img.id, tex));
        }
        if let Some((_, tex)) = &self.cursor_tex {
            ui.painter().with_clip_rect(video).image(
                tex.id(),
                egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}
