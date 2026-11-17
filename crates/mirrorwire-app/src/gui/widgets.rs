//! Stateless egui widgets and panels for the GUI window (moved out of `gui/mod.rs`).

use super::{FirewallFix, MirrorApp};
use crate::applog::LogStore;
use anyhow::Result;
use eframe::egui;
use mirrorwire_net::diag::{ConnectStep, FirewallHint, IncomingAttempt, StepState};
use mirrorwire_net::firewall::{Notice, OpenResult, UserConfirmed, NOT_CHANGED_NOTE};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) fn qr_color_image(uri: &str) -> Result<egui::ColorImage> {
    let (n, modules) = mirrorwire_net::pairing::qr_matrix(uri)?;
    let quiet = 4;
    let side = n + quiet * 2;
    let mut rgba = vec![255u8; side * side * 4];
    for y in 0..n {
        for x in 0..n {
            if modules.get(y * n + x).copied().unwrap_or(false) {
                let i = ((y + quiet) * side + (x + quiet)) * 4;
                if let Some(px) = rgba.get_mut(i..i + 3) {
                    px.fill(0);
                }
            }
        }
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [side, side],
        &rgba,
    ))
}

pub(super) fn step_row(ui: &mut egui::Ui, step: &ConnectStep) {
    let (mark, color) = match step.state {
        StepState::Pending => ("·", egui::Color32::GRAY),
        StepState::Active => ("…", egui::Color32::from_rgb(230, 180, 40)),
        StepState::Ok => ("✓", egui::Color32::from_rgb(0, 180, 80)),
        StepState::Failed => ("✗", egui::Color32::LIGHT_RED),
    };
    ui.horizontal(|ui| {
        ui.colored_label(color, mark);
        ui.monospace(step.at.clone().unwrap_or_else(|| "        ".into()));
        ui.strong(step.id.label());
        if !step.detail.is_empty() {
            ui.label(&step.detail);
        }
    });
}

/// Copy through egui's own clipboard (winit: smithay-clipboard on Wayland,
/// the system pasteboard on macOS). It reports no errors back, so the note
/// keeps the manual fallback in view.
pub(super) fn copy_with_feedback(
    ctx: &egui::Context,
    text: String,
    note: &mut Option<(Instant, bool, String)>,
) {
    ctx.copy_text(text);
    *note = Some((
        Instant::now(),
        true,
        "Copied. If pasting doesn't work, the text is selectable — use Cmd/Ctrl+C.".into(),
    ));
}

pub(super) fn show_copy_note(ui: &mut egui::Ui, note: &Option<(Instant, bool, String)>) {
    let Some((at, ok, msg)) = note else { return };
    if at.elapsed().as_secs() >= 4 {
        return;
    }
    let color = if *ok {
        egui::Color32::from_rgb(0, 180, 80)
    } else {
        egui::Color32::from_rgb(230, 160, 40)
    };
    ui.colored_label(color, msg);
}

pub(super) fn log_panel(
    ui: &mut egui::Ui,
    log: &LogStore,
    note: &mut Option<(Instant, bool, String)>,
) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.weak(format!("Log: {}", log.path().display()));
        if ui.button("Copy log").clicked() {
            copy_with_feedback(ui.ctx(), log.snapshot(), note);
        }
        show_copy_note(ui, note);
    });
    if let Some(err) = log.open_error() {
        ui.colored_label(egui::Color32::LIGHT_RED, err);
    }
    let text = log.snapshot();
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(text).monospace().size(11.0))
                    .selectable(true)
                    .wrap(),
            );
        });
}

pub(super) fn firewall_panel(
    ui: &mut egui::Ui,
    app: &mut MirrorApp,
    hint: Option<&FirewallHint>,
    viewer_reached_us: bool,
) {
    let Some(hint) = hint else { return };

    // A successful "Open port for me…" re-runs the (unprivileged) probe once.
    if let Some(slot) = &app.firewall_fix {
        if let Ok(mut g) = slot.lock() {
            if let FirewallFix::Ok(msg) = &*g {
                let msg = msg.clone();
                if let Some(h) = &app.presenting {
                    h.refresh_firewall();
                }
                *g = FirewallFix::Applied(msg);
            }
        }
    }
    let fix = app.firewall_fix.as_ref().and_then(|s| {
        s.lock().ok().map(|g| match &*g {
            FirewallFix::Running => FirewallFix::Running,
            FirewallFix::Ok(m) | FirewallFix::Applied(m) => FirewallFix::Applied(m.clone()),
            FirewallFix::NotChanged => FirewallFix::NotChanged,
            FirewallFix::Err(m) => FirewallFix::Err(m.clone()),
        })
    });
    let green = egui::Color32::from_rgb(0, 180, 80);

    let notice = hint.notice(viewer_reached_us);
    match notice {
        Notice::Hidden => return,
        Notice::Ok => {
            ui.add_space(4.0);
            if let Some(FirewallFix::Applied(msg)) = &fix {
                ui.colored_label(green, msg);
            } else {
                ui.weak("Firewall OK");
            }
            return;
        }
        Notice::Unknown | Notice::Blocked => {}
    }

    ui.add_space(8.0);
    let color = if notice == Notice::Blocked {
        egui::Color32::from_rgb(230, 160, 40)
    } else {
        egui::Color32::GRAY
    };
    ui.colored_label(color, &hint.message);
    ui.label("To open it, run this in a terminal:");
    ui.horizontal(|ui| {
        ui.monospace(&hint.command);
        if ui.button("Copy command").clicked() {
            copy_with_feedback(ui.ctx(), hint.command.clone(), &mut app.copy_note);
        }
        show_copy_note(ui, &app.copy_note);
    });
    egui::CollapsingHeader::new("Other systems, and keeping it after a reboot")
        .id_salt("firewall-guide")
        .default_open(false)
        .show(ui, |ui| {
            ui.weak(&hint.guide);
        });

    if hint.can_autofix {
        let busy = matches!(fix, Some(FirewallFix::Running));
        ui.horizontal(|ui| {
            let label = if busy {
                "Waiting for the password dialog…"
            } else {
                "Open port for me…"
            };
            // The only place that may cause a password dialog (decision D14).
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                let port = hint.port;
                let confirmed = UserConfirmed::from_button_click();
                let slot = Arc::new(Mutex::new(FirewallFix::Running));
                app.firewall_fix = Some(slot.clone());
                let ctx = ui.ctx().clone();
                std::thread::Builder::new()
                    .name("firewall-open".into())
                    .spawn(move || {
                        let result = FirewallHint::open_port(port, confirmed);
                        if let Ok(mut g) = slot.lock() {
                            *g = match result {
                                OpenResult::Opened(msg) => FirewallFix::Ok(msg),
                                OpenResult::NotChanged => FirewallFix::NotChanged,
                                OpenResult::Failed(msg) => FirewallFix::Err(msg),
                            };
                        }
                        ctx.request_repaint();
                    })
                    .ok();
            }
        });
        ui.weak("This asks for your admin password once. If you cancel, nothing changes and MirrorWire won't ask again.");
    }

    match &fix {
        Some(FirewallFix::Applied(msg)) => {
            ui.colored_label(green, msg);
        }
        Some(FirewallFix::NotChanged) => {
            ui.weak(NOT_CHANGED_NOTE);
        }
        Some(FirewallFix::Err(msg)) => {
            ui.colored_label(egui::Color32::LIGHT_RED, msg);
            ui.weak("You can open the port yourself with the command above.");
        }
        _ => {}
    }
}

pub(super) fn attempts_panel(ui: &mut egui::Ui, attempts: &[IncomingAttempt]) {
    ui.add_space(8.0);
    ui.strong("Connection attempts");
    if attempts.is_empty() {
        ui.weak("No connection attempts received yet");
        return;
    }
    for a in attempts.iter().rev().take(12) {
        let color = if a.ok {
            egui::Color32::from_rgb(0, 180, 80)
        } else {
            egui::Color32::LIGHT_RED
        };
        let name = a.viewer_name.as_deref().unwrap_or("unknown viewer");
        ui.colored_label(
            color,
            format!("{}  {}  {}  {}", a.at, a.peer, name, a.outcome),
        );
    }
}

/// Presenter status line for forward error correction.
pub(super) fn fec_status_line(
    fec: &mirrorwire_net::transport::LinkState,
    viewer_connected: bool,
) -> String {
    let mode = fec.mode.as_string();
    if !viewer_connected {
        return format!("FEC: {mode}");
    }
    match fec.overheads() {
        None => "FEC: off".into(),
        Some((delta, key)) => format!(
            "FEC: {mode}  ·  parity +{:.0}% delta / +{:.0}% keyframes{}  ·  viewer loss {:.1}%  ·  rebuilt {:.0} packets/s  ·  unrecoverable frames {}",
            delta * 100.0,
            key * 100.0,
            if fec.boost_pct > 0.0 { format!(" (raised {:.0} points after lost frames)", fec.boost_pct) } else { String::new() },
            fec.loss_pct,
            fec.recovered_per_s,
            fec.unrecoverable
        ),
    }
}
