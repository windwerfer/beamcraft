//! A large-window congestion controller for a trusted LAN.
//!
//! Quinn's default BBR/Cubic pacing tracks the application's average send
//! rate, so a 40 Mbit/s encode is spread across the whole frame interval
//! (~40 ms of "reassembly" for a 1080p frame). On a LAN we want to burst
//! the fragments as fast as the path allows, then idle until the next frame.

use quinn::congestion::{Controller, ControllerFactory, ControllerMetrics};
use quinn_proto::RttEstimator;
use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

/// 4 MiB, enough for several 1080p I-frames on the wire at once.
pub const LAN_WINDOW: u64 = 4 * 1024 * 1024;
/// The window never goes below this, however often congestion is persistent.
const MIN_WINDOW: u64 = 256 * 1024;
/// 500 Mbit/s pacing: a normal P-frame (tens of KB) still leaves as a burst
/// in under a millisecond, but a large IDR or scene-change frame is spread
/// over a few ms instead of hitting a Wi-Fi access point as one multi-MB
/// burst (which overflows its queue, loses fragments, triggers a keyframe
/// request and so another big frame).
const LAN_PACING_BITS_PER_SEC: u64 = 500_000_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct LanBurstConfig;

pub fn factory() -> Arc<LanBurstConfig> {
    Arc::new(LanBurstConfig)
}

#[derive(Clone)]
struct LanBurst {
    window: u64,
}

impl LanBurst {
    /// Persistent congestion (every packet of a long period lost, e.g. Wi-Fi roaming or a
    /// stalled radio) halves the window.
    fn halve(&mut self) {
        self.window = (self.window / 2).max(MIN_WINDOW);
    }

    /// Acknowledged bytes give the window back, one byte per acknowledged byte (slow start),
    /// up to [`LAN_WINDOW`]. Before this the window only ever shrank: after a few stalls it sat
    /// at 256 KiB for good and a keyframe plus parity (up to ~400 KB) was window-bound, adding
    /// round trips to every keyframe. Not limited to acks that were not application limited:
    /// the video is bursty, so most acks are, and the window is the only thing that matters in
    /// the burst; the cap keeps it no larger than it started.
    fn grow(&mut self, acked_bytes: u64) {
        self.window = self.window.saturating_add(acked_bytes).min(LAN_WINDOW);
    }
}

impl Controller for LanBurst {
    fn on_ack(
        &mut self,
        _now: Instant,
        _sent: Instant,
        bytes: u64,
        _app_limited: bool,
        _rtt: &RttEstimator,
    ) {
        self.grow(bytes);
    }

    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        is_persistent_congestion: bool,
        _lost_bytes: u64,
    ) {
        if is_persistent_congestion {
            self.halve();
        }
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        self.window
    }

    fn metrics(&self) -> ControllerMetrics {
        let mut metrics = ControllerMetrics::default();
        metrics.congestion_window = self.window;
        metrics.pacing_rate = Some(LAN_PACING_BITS_PER_SEC);
        metrics
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        LAN_WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl ControllerFactory for LanBurstConfig {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(LanBurst { window: LAN_WINDOW })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_comes_back_after_persistent_congestion() {
        let mut c = LanBurst { window: LAN_WINDOW };
        for _ in 0..10 {
            c.on_congestion_event(Instant::now(), Instant::now(), true, 1_000);
        }
        assert_eq!(c.window(), MIN_WINDOW, "never below the minimum");
        // Ordinary congestion (not persistent) leaves the window alone.
        c.on_congestion_event(Instant::now(), Instant::now(), false, 1_000);
        assert_eq!(c.window(), MIN_WINDOW);
        // Acknowledged bytes grow it back, up to the original size and no further.
        c.grow(1_000_000);
        assert_eq!(c.window(), MIN_WINDOW + 1_000_000);
        c.grow(u64::MAX);
        assert_eq!(c.window(), LAN_WINDOW);
        assert_eq!(c.metrics().congestion_window, LAN_WINDOW);
    }
}
