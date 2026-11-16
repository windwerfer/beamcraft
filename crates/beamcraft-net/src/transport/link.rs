//! Presenter link control: the FEC setting, the viewer's link reports and the
//! adaptive bitrate budget derived from them.

use beamcraft_proto::control::QueueDelay;
use beamcraft_proto::fec::FecMode;
use beamcraft_proto::rate::{AdaptiveBitrate, Sample};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ceiling for [`FecState::boost_pct`].
const MAX_FEC_BOOST: f32 = 15.0;

/// Presenter-side link control, shared with the app: the FEC setting, what
/// the current viewer last reported about the link, and the adaptive
/// bitrate budget (video + parity) derived from it.
#[derive(Debug)]
pub struct FecControl {
    state: Mutex<FecState>,
    started: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct FecState {
    pub mode: FecMode,
    /// Smoothed packet loss the viewer reports (percent); drives auto mode.
    pub loss_pct: f32,
    /// Data packets the viewer rebuilt from parity, per second.
    pub recovered_per_s: f32,
    /// Frames the viewer could not rebuild since it connected.
    pub unrecoverable: u64,
    /// Extra parity (percentage points, auto mode) added while frames are
    /// still lost: +5 per report with unrecoverable frames, -1 per clean
    /// one. It does not grow while the loss looks like congestion (more
    /// parity would only add load).
    pub boost_pct: f32,
    pub reports: u64,
    last_report: Option<Instant>,
    /// Adaptive bitrate on (budget follows the link) or off (budget = setting).
    pub adaptive: bool,
    /// AIMD controller; its ceiling is the user's bitrate setting.
    pub rate: AdaptiveBitrate,
    /// Latest QUIC RTT estimate (ms); shown, not used by the controller.
    pub rtt_ms: f32,
    /// What the presenter sent on the wire (video + parity, kbit/s) in the
    /// last send window; the budget only grows while it is used.
    pub sent_kbps: Option<f32>,
    /// The viewer's latest one-way queueing delay.
    pub queue: Option<QueueDelay>,
}

impl Default for FecState {
    fn default() -> Self {
        Self {
            mode: FecMode::default(),
            loss_pct: 0.0,
            recovered_per_s: 0.0,
            unrecoverable: 0,
            boost_pct: 0.0,
            reports: 0,
            last_report: None,
            adaptive: true,
            rate: AdaptiveBitrate::new(40_000),
            rtt_ms: 0.0,
            sent_kbps: None,
            queue: None,
        }
    }
}

impl FecState {
    /// Overhead currently applied to (delta, key) frames, or `None` when FEC
    /// is off.
    pub fn overheads(&self) -> Option<(f32, f32)> {
        Some((
            self.mode.overhead(self.loss_pct, self.boost_pct, false)?,
            self.mode.overhead(self.loss_pct, self.boost_pct, true)?,
        ))
    }

    /// Total budget on the wire (kbit/s): the adaptive one, or the setting.
    pub fn budget_kbps(&self) -> u32 {
        if self.adaptive {
            self.rate.budget_kbps()
        } else {
            self.rate.ceiling_kbps()
        }
    }

    /// Presenter status line, e.g. `Bitrate: target 18.0 of max 40.0 Mbit/s
    /// (adaptive)  ·  video 15.7 + FEC parity`.
    pub fn bitrate_line(&self) -> String {
        let mode = if self.adaptive { "adaptive" } else { "fixed" };
        let mut line = format!(
            "Bitrate: target {:.1} of max {:.1} Mbit/s ({mode})",
            self.budget_kbps() as f32 / 1000.0,
            self.rate.ceiling_kbps() as f32 / 1000.0,
        );
        if self.overheads().is_some() {
            line += &format!(
                "  ·  video {:.1} + FEC parity",
                self.encoder_kbps() as f32 / 1000.0
            );
        }
        if self.adaptive && self.rate.congested() {
            line += "  ·  lowered: the link is congested";
        }
        if let Some(cut) = self.rate.last_cut().filter(|_| self.adaptive) {
            line += &format!("  ·  last cut: {}", cut.reason());
        }
        line
    }

    /// Encoder target: the budget minus room for delta-frame parity.
    pub fn encoder_kbps(&self) -> u32 {
        beamcraft_proto::rate::encoder_kbps(self.budget_kbps(), self.overheads().map(|o| o.0))
    }
}

impl FecControl {
    pub fn new(mode: FecMode) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FecState {
                mode,
                ..Default::default()
            }),
            started: Instant::now(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FecState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn snapshot(&self) -> FecState {
        *self.lock()
    }

    pub fn set_mode(&self, mode: FecMode) {
        self.lock().mode = mode;
    }

    pub fn set_adaptive(&self, on: bool) {
        let mut s = self.lock();
        if s.adaptive != on {
            s.adaptive = on;
            s.rate.reset();
        }
    }

    /// The user's bitrate setting (kbit/s): the adaptive ceiling.
    pub fn set_ceiling(&self, kbps: u32) {
        self.lock().rate.set_ceiling(kbps);
    }

    /// The encoder target for the next frames (kbit/s), see [`FecState::encoder_kbps`].
    pub fn encoder_kbps(&self) -> u32 {
        self.lock().encoder_kbps()
    }

    pub(super) fn viewer_connected(&self) {
        let mut s = self.lock();
        let (mode, adaptive, mut rate) = (s.mode, s.adaptive, s.rate);
        rate.reset();
        *s = FecState {
            mode,
            adaptive,
            rate,
            ..Default::default()
        };
    }

    pub(super) fn overhead(&self, keyframe: bool) -> Option<f32> {
        let s = self.lock();
        s.mode.overhead(s.loss_pct, s.boost_pct, keyframe)
    }

    pub(super) fn on_report(
        &self,
        loss_permille: u32,
        recovered: u32,
        unrecoverable: u32,
        queue: Option<QueueDelay>,
    ) {
        let now_ms = self.now_ms();
        let mut s = self.lock();
        let loss = (loss_permille.min(1000) as f32) / 10.0;
        // Rise fast, decay slowly: a loss burst should get more parity at once.
        let w = if loss > s.loss_pct { 0.5 } else { 0.15 };
        s.loss_pct = if s.reports == 0 {
            loss
        } else {
            s.loss_pct + w * (loss - s.loss_pct)
        };
        let now = Instant::now();
        let secs = s
            .last_report
            .map(|t| now.duration_since(t).as_secs_f32())
            .filter(|d| *d > 0.05)
            .unwrap_or(0.5);
        s.recovered_per_s = recovered as f32 / secs;
        s.unrecoverable += unrecoverable as u64;
        // The controller runs even when adaptive is off, so switching it on
        // starts from what it learned; it only decides the budget when on.
        let sent_kbps = s.sent_kbps;
        if queue.is_some() {
            s.queue = queue;
        }
        s.rate.on_sample(
            Sample {
                loss_pct: Some(loss),
                backlog_drops: 0,
                sent_kbps,
                queue,
            },
            now_ms,
        );
        log_cut(&s);
        s.boost_pct = if unrecoverable > 0 && !s.rate.congested() {
            (s.boost_pct + 5.0 * unrecoverable.min(3) as f32).min(MAX_FEC_BOOST)
        } else {
            (s.boost_pct - 1.0).max(0.0)
        };
        s.reports += 1;
        s.last_report = Some(now);
    }

    /// The presenter's own view, once per send window: frames dropped for
    /// backlog, the wire rate, and the RTT (for the status line only).
    pub(super) fn on_send_window(&self, backlog_drops: u32, sent_kbps: f32, rtt: Duration) {
        let now_ms = self.now_ms();
        let mut s = self.lock();
        s.sent_kbps = Some(sent_kbps);
        s.rtt_ms = rtt.as_secs_f32() * 1000.0;
        if backlog_drops > 0 {
            s.rate.on_sample(
                Sample {
                    loss_pct: None,
                    backlog_drops,
                    sent_kbps: Some(sent_kbps),
                    queue: None,
                },
                now_ms,
            );
            log_cut(&s);
        }
    }
}

/// Log the cut the latest sample made (only when adaptive bitrate is on,
/// since otherwise the budget does not follow the controller).
fn log_cut(s: &FecState) {
    if let Some(cut) = s.rate.cut_by_last_sample().filter(|_| s.adaptive) {
        tracing::info!("adaptive bitrate: {cut}");
    }
}

/// `bitrate 18.0 of 40.0 Mbit/s (adaptive), encoder 15.2, rtt 6 ms`
pub fn rate_summary(s: &FecState) -> String {
    format!(
        "bitrate {:.1} of {:.1} Mbit/s ({}), encoder {:.1}, rtt {:.0} ms{}{}",
        s.budget_kbps() as f32 / 1000.0,
        s.rate.ceiling_kbps() as f32 / 1000.0,
        if s.adaptive { "adaptive" } else { "fixed" },
        s.encoder_kbps() as f32 / 1000.0,
        s.rtt_ms,
        s.queue.map_or(String::new(), |q| format!(", viewer {q}")),
        if s.adaptive && s.rate.congested() {
            ", congested"
        } else {
            ""
        }
    )
}
