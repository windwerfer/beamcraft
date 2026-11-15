//! One-way queueing delay, measured by the viewer from the video packets.
//!
//! Every packet carries the presenter's `sent_us`. For the first packet that
//! arrives of each new frame, `arrival - sent_us` is the one-way delay plus
//! the (unknown, constant) clock offset between the two machines. The lowest
//! value seen recently is the empty-queue baseline, so `delay - baseline` is
//! the time the packet waited in a queue on the way (Wi-Fi, router, the
//! viewer's socket buffer). The clock offset cancels out; drift between the
//! clocks (a few ppm, ~1 ms per minute) is absorbed by forgetting the
//! baseline after [`BASE_SECS`] seconds.
//!
//! The first packet of a frame is used, not the last, because the last one
//! also waits for the frame's own packets to cross the link (a big keyframe
//! takes 10–20 ms on a 20 Mbit/s link), which is not a queue left over from
//! earlier frames. If the link keeps up, that queue drains between frames
//! and the first packet sees the baseline; once the presenter sends more than
//! the link carries, it grows from frame to frame, before any packet is lost.
//!
//! Per report window the viewer sends the mean and max queueing delay and its
//! trend (least-squares slope over the window's frames, µs per second) in
//! `link_report`.

/// Seconds of per-second minima kept for the baseline.
pub const BASE_SECS: usize = 30;
/// A queueing delay above this is taken as a clock step (NTP), not a queue:
/// the baseline starts again.
pub const CLOCK_STEP_US: i64 = 2_000_000;

/// The window's queueing delay, for `link_report`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DelaySummary {
    /// Frames measured in the window.
    pub frames: u32,
    pub mean_us: u32,
    pub max_us: u32,
    /// Least-squares slope of the queueing delay over the window (µs of
    /// delay per second); 0 with fewer than 3 frames.
    pub trend_us_per_s: i32,
}

#[derive(Debug, Clone, Default)]
pub struct DelayMeter {
    newest_frame: Option<u32>,
    /// (second, lowest raw delay in it), oldest first.
    minima: Vec<(u64, i64)>,
    window: Window,
}

#[derive(Debug, Clone, Default)]
struct Window {
    start_us: Option<u64>,
    n: u32,
    sum: f64,
    max: i64,
    st: f64,
    stt: f64,
    sq: f64,
    stq: f64,
}

impl DelayMeter {
    /// Feed every video packet; only the first one that arrives of each new
    /// frame is measured (reordered packets of older frames are ignored).
    pub fn on_packet(&mut self, frame_id: u32, sent_us: u64, now_us: u64) {
        if self.newest_frame.is_some_and(|f| frame_id <= f) {
            return;
        }
        self.newest_frame = Some(frame_id);
        let (Ok(now), Ok(sent)) = (i64::try_from(now_us), i64::try_from(sent_us)) else {
            return;
        };
        let raw = now.saturating_sub(sent);
        let sec = now_us / 1_000_000;
        match self.minima.last_mut() {
            Some((s, m)) if *s == sec => *m = (*m).min(raw),
            _ => {
                self.minima.push((sec, raw));
                let keep_from = sec.saturating_sub(BASE_SECS as u64 - 1);
                self.minima.retain(|(s, _)| *s >= keep_from);
            }
        }
        let base = self.minima.iter().map(|(_, m)| *m).min().unwrap_or(raw);
        let mut q = raw.saturating_sub(base);
        if q > CLOCK_STEP_US {
            self.minima.clear();
            self.minima.push((sec, raw));
            q = 0;
        }
        let w = &mut self.window;
        let start = *w.start_us.get_or_insert(now_us);
        let t = now_us.saturating_sub(start) as f64 / 1e6;
        let qf = q as f64;
        w.n = w.n.saturating_add(1);
        w.sum += qf;
        w.max = w.max.max(q);
        w.st += t;
        w.stt += t * t;
        w.sq += qf;
        w.stq += t * qf;
    }

    /// The window since the previous call, or `None` if no frame arrived.
    pub fn take(&mut self) -> Option<DelaySummary> {
        let w = std::mem::take(&mut self.window);
        if w.n == 0 {
            return None;
        }
        let n = w.n as f64;
        let denom = n * w.stt - w.st * w.st;
        let trend = if w.n >= 3 && denom > 1e-9 {
            (n * w.stq - w.st * w.sq) / denom
        } else {
            0.0
        };
        let to_u32 = |v: f64| v.clamp(0.0, u32::MAX as f64) as u32;
        Some(DelaySummary {
            frames: w.n,
            mean_us: to_u32(w.sum / n),
            max_us: to_u32(w.max as f64),
            trend_us_per_s: trend.clamp(i32::MIN as f64, i32::MAX as f64) as i32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames every 16 ms; `delay(i)` is the one-way delay of frame i
    /// (plus a 5 s clock offset, which must cancel out).
    fn run(m: &mut DelayMeter, from: u32, n: u32, delay: impl Fn(u32) -> u64) {
        for i in from..from + n {
            let sent = 1_000_000_000 + i as u64 * 16_000;
            let now = sent + 5_000_000 + delay(i);
            // Two packets per frame; only the first counts.
            m.on_packet(i, sent, now);
            m.on_packet(i, sent, now + 3_000);
        }
    }

    #[test]
    fn flat_delay_has_no_queue_and_no_trend() {
        let mut m = DelayMeter::default();
        run(&mut m, 0, 30, |i| 4_000 + (i as u64 % 3) * 100);
        let s = m.take().unwrap();
        assert_eq!(s.frames, 30);
        assert!(s.mean_us <= 200 && s.max_us <= 200, "{s:?}");
        assert!(s.trend_us_per_s.abs() < 500, "{s:?}");
        assert_eq!(m.take(), None, "window was reset");
    }

    #[test]
    fn a_growing_queue_shows_as_delay_and_trend() {
        let mut m = DelayMeter::default();
        run(&mut m, 0, 30, |_| 4_000);
        m.take();
        // 1 ms more per 16 ms frame; arrivals are 17 ms apart, so 58.8 ms of
        // queue per second of arrival time.
        run(&mut m, 30, 30, |i| 4_000 + (i as u64 - 29) * 1_000);
        let s = m.take().unwrap();
        assert!((14_000..=17_000).contains(&s.mean_us), "{s:?}");
        assert_eq!(s.max_us, 30_000);
        assert!((57_000..=61_000).contains(&s.trend_us_per_s), "{s:?}");
        // Draining: negative trend.
        run(&mut m, 60, 20, |i| 34_000 - (i as u64 - 59) * 1_500);
        assert!(m.take().unwrap().trend_us_per_s < -50_000);
    }

    #[test]
    fn old_frames_and_clock_steps_are_handled() {
        let mut m = DelayMeter::default();
        run(&mut m, 10, 5, |_| 4_000);
        // A late packet of an older frame is not a measurement.
        m.on_packet(3, 1_000_000_000, 1_000_000_000 + 5_900_000);
        assert_eq!(m.take().unwrap().max_us, 0);
        // The viewer's clock jumps 10 s ahead: no 10 s "queue".
        run(&mut m, 20, 5, |_| 10_004_000);
        assert!(m.take().unwrap().max_us < 1_000);
        // Hostile timestamps: no panic.
        m.on_packet(100, u64::MAX, 0);
        m.on_packet(101, 0, u64::MAX);
        m.take();
    }

    #[test]
    fn baseline_follows_drift_after_it_ages_out() {
        let mut m = DelayMeter::default();
        run(&mut m, 0, 60, |_| 4_000);
        // The path gets 3 ms longer for good (or the clocks drifted): after
        // BASE_SECS the old minimum is forgotten.
        let per_s = 1_000_000 / 16_000;
        run(&mut m, 60, (BASE_SECS as u32 + 2) * per_s, |_| 7_000);
        m.take();
        run(&mut m, 60 + (BASE_SECS as u32 + 2) * per_s, 10, |_| 7_000);
        assert!(m.take().unwrap().max_us < 100);
    }
}
