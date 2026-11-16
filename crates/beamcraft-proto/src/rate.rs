//! Adaptive bitrate for the presenter: an AIMD controller driven by the
//! viewer's `link_report` (packet loss and one-way queueing delay) and the
//! presenter's own send queue.
//!
//! The controller owns the **total budget** on the wire (video + FEC
//! parity). [`encoder_kbps`] turns it into the encoder target by leaving
//! room for the parity the FEC setting will add, so FEC never pushes the
//! stream above the budget (on congested Wi-Fi, extra parity only made the
//! loss worse).
//!
//! * Congestion: loss clearly above what the link loses anyway, sustained
//!   for two reports (or one heavy report), a queue building up, or the
//!   presenter dropping frames for backlog. Then the budget is cut
//!   multiplicatively (×0.85, ×0.75 heavy, ×0.6 severe), at most once per
//!   second, never below the floor.
//! * Queue: the viewer's one-way queueing delay ([`crate::delay`]) cuts as
//!   soon as one report shows a queue
//!   building (mean ≥ [`QUEUE_MS`] or max ≥ [`QUEUE_MAX_MS`], and rising
//!   ≥ [`QUEUE_TREND_MS_PER_S`]) or
//!   standing (≥ [`QUEUE_STANDING_MS`], not in the 2 s after a cut while it
//!   drains); that cut also goes to 90 % of what
//!   was sent, since the link carried less. This comes before the loss: on
//!   the MTK tablet (2026-11-15) the queue rose for ~1 s before packets were
//!   dropped. Loss while no frame waited [`QUEUE_LOSS_MS`] in a queue is
//!   random loss: FEC covers it and the rate stays (except ≥ 20 points).
//!   The QUIC RTT is not used: it reacts later (ack delay, smoothing) and
//!   says nothing the queue does not.
//! * Start: the budget begins at [`START_KBPS`] (or the ceiling if lower)
//!   and grows ×1.25 per second until the first cut (slow start), so a
//!   new viewer on weak Wi-Fi is not hit with the full ceiling at once
//!   (measured 2026-11-15 on the MTK tablet: starting at 40 Mbit/s on a
//!   ~20–25 Mbit/s link lost 50–100 frames in the first 3 s).
//! * Clean (little loss, no queue): the budget climbs additively by 4 % of
//!   the ceiling per second, starting 4 s after the last cut.
//! * Hysteresis: each cut remembers what the link did not carry (the
//!   budget, or what was sent if less). From 90 % to 115 % of that value
//!   the climb is a quarter as fast, and from 90 % up loss above the
//!   background counts as congestion even with a flat queue. A shallow
//!   bottleneck buffer (≤ 50 ms: a router, USB) drops before a queue shows,
//!   so without this each overrun lost ~23 % of a report's packets.
//! * Only while it is used: the budget does not grow while the presenter
//!   sends less than 80 % of it (static or simple content). Otherwise it
//!   drifts far above what was ever proven to fit, and the next busy frame
//!   overruns the link (budget 26 at 12 Mbit/s sent, then 24 Mbit/s into a
//!   ~20 Mbit/s link: 15–50 frames lost per episode on the tablet).
//! * Loss that does not go away when the rate is cut is learned as the
//!   link's random loss (Wi-Fi retries, interference); FEC handles it, and
//!   it does not keep pushing the rate down.
//!
//! Time comes in as milliseconds so the logic is testable without a clock.

use crate::control::QueueDelay;

/// Lowest budget the controller goes to (kbit/s), or the ceiling if lower.
pub const FLOOR_KBPS: u32 = 3_000;
/// Budget a new viewer starts with (kbit/s), or the ceiling if lower.
pub const START_KBPS: u32 = 10_000;
/// Slow-start growth per second until the first cut.
const SLOW_START_PER_S: f32 = 1.25;
/// The budget only grows while at least this share of it is sent.
const USED_SHARE: f32 = 0.8;
/// Smallest additive step per second (kbit/s).
const MIN_STEP_KBPS: f32 = 250.0;
const MIN_DECREASE_GAP_MS: u64 = 1_000;
const INCREASE_AFTER_CUT_MS: u64 = 4_000;
/// How long after a cut the loss is watched to see whether it followed the rate.
const PROBE_MS: u64 = 3_000;
const MAX_RANDOM_LOSS: f32 = 15.0;
/// Queue trigger: a mean
/// queue of at least `QUEUE_MS` (or one frame waiting `QUEUE_MAX_MS`) that
/// is still rising by `QUEUE_TREND_MS_PER_S`…
pub const QUEUE_MS: f32 = 25.0;
pub const QUEUE_MAX_MS: f32 = 50.0;
pub const QUEUE_TREND_MS_PER_S: f32 = 30.0;
/// …or a standing queue of at least this much, except within
/// [`QUEUE_DRAIN_MS`] of a cut.
pub const QUEUE_STANDING_MS: f32 = 60.0;
pub const QUEUE_DRAIN_MS: u64 = 2_000;
/// Loss only counts as congestion when a frame waited at least this long in
/// a queue (this report or the one before); otherwise it is random loss for
/// FEC. Loss of [`SEVERE_EXCESS`] points or more cuts regardless.
pub const QUEUE_LOSS_MS: f32 = 40.0;
const SEVERE_EXCESS: f32 = 20.0;
/// A queue cut goes at least this far below what was sent (the link
/// carried less than that, or the queue would not have grown).
const QUEUE_CUT_OF_SENT: f32 = 0.9;
/// From this share of the last bad budget up, loss above the background is
/// congestion even with a flat queue, and the budget climbs slowly.
const NEAR_BAD_SHARE: f32 = 0.9;
/// Above this multiple of the last bad budget the climb is at full speed
/// again (the link got better).
const PAST_BAD_SHARE: f32 = 1.15;
/// The additive step near the last bad budget, as a share of the normal one.
const NEAR_BAD_STEP: f32 = 0.25;

/// One feedback sample. `loss_pct` is `None` when only the presenter's own
/// view is known (send window without a viewer report).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    pub loss_pct: Option<f32>,
    /// Delta frames the presenter dropped because the send queue was full.
    pub backlog_drops: u32,
    /// What the presenter actually sent (video + parity, kbit/s) over the
    /// last window; `None` when unknown (then the budget counts as used).
    pub sent_kbps: Option<f32>,
    /// The viewer's one-way queueing delay over its report window; `None`
    /// in the presenter's own samples and when no frame arrived.
    pub queue: Option<QueueDelay>,
}

/// What triggered a cut (the first that applies, in this order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CutTrigger {
    /// The presenter dropped delta frames because its send queue was full.
    Backlog { drops: u32 },
    /// Loss far above the link's background loss (one report is enough).
    HeavyLoss,
    /// Loss above the background loss for two reports in a row.
    Loss,
    /// The viewer's one-way queueing delay is building up or standing.
    Queue,
}

/// One budget cut and the values that caused it, for logs and the GUI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub at_ms: u64,
    pub trigger: CutTrigger,
    pub from_kbps: u32,
    pub to_kbps: u32,
    /// Reported loss (percent), if the sample had one.
    pub loss_pct: Option<f32>,
    /// Loss treated as background at the time (percent).
    pub random_loss_pct: f32,
    /// The viewer's queueing delay in that report.
    pub queue: Option<QueueDelay>,
}

impl Cut {
    /// Short reason, e.g. `loss 5.2 % (background 1.8 %)` or
    /// `queue 30 ms (max 60, +100 ms/s)`.
    pub fn reason(&self) -> String {
        let loss = || {
            format!(
                "loss {:.1} % (background {:.1} %)",
                self.loss_pct.unwrap_or(0.0),
                self.random_loss_pct
            )
        };
        match self.trigger {
            CutTrigger::Backlog { drops } => format!("send queue full, {drops} frames dropped"),
            CutTrigger::HeavyLoss => format!("heavy {}", loss()),
            CutTrigger::Loss => loss(),
            CutTrigger::Queue => self
                .queue
                .map_or_else(|| "queue".to_owned(), |q| q.to_string()),
        }
    }
}

impl std::fmt::Display for Cut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cut {:.1} -> {:.1} Mbit/s: {}",
            self.from_kbps as f32 / 1000.0,
            self.to_kbps as f32 / 1000.0,
            self.reason()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveBitrate {
    ceiling_kbps: u32,
    budget_kbps: f32,
    last_sample_ms: Option<u64>,
    last_cut_ms: Option<u64>,
    lossy_streak: u32,
    congested: bool,
    /// Loss the link shows regardless of the rate (learned, decays).
    random_loss_pct: f32,
    /// Loss when the last cut happened, and the samples since (probe).
    probe: Option<Probe>,
    /// Growing multiplicatively until the first cut.
    slow_start: bool,
    /// The most recent cut, kept until the next one.
    last_cut: Option<Cut>,
    /// Set by `on_sample` when that sample cut the budget.
    cut_now: bool,
    /// The previous sample's queue reached [`QUEUE_LOSS_MS`].
    queue_was_high: bool,
    /// What the link did not carry at the last cut: the budget then, or
    /// what was sent if less. Near it the climb is slow and loss counts.
    last_bad_kbps: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Probe {
    loss_before: f32,
    until_ms: u64,
    sum: f32,
    n: u32,
}

impl AdaptiveBitrate {
    /// Starts at [`START_KBPS`] (or the ceiling if lower), in slow start.
    pub fn new(ceiling_kbps: u32) -> Self {
        Self::with_start(ceiling_kbps, START_KBPS)
    }

    /// Like [`Self::new`] with another start budget (clamped to floor..ceiling).
    pub fn with_start(ceiling_kbps: u32, start_kbps: u32) -> Self {
        let ceiling_kbps = ceiling_kbps.max(1);
        let floor = FLOOR_KBPS.min(ceiling_kbps);
        Self {
            ceiling_kbps,
            budget_kbps: start_kbps.clamp(floor, ceiling_kbps) as f32,
            last_sample_ms: None,
            last_cut_ms: None,
            lossy_streak: 0,
            congested: false,
            random_loss_pct: 0.0,
            probe: None,
            slow_start: true,
            last_cut: None,
            cut_now: false,
            queue_was_high: false,
            last_bad_kbps: None,
        }
    }

    /// The most recent cut and why it happened.
    pub fn last_cut(&self) -> Option<Cut> {
        self.last_cut
    }

    /// The cut made by the latest `on_sample`, if it made one.
    pub fn cut_by_last_sample(&self) -> Option<Cut> {
        self.last_cut.filter(|_| self.cut_now)
    }

    pub fn ceiling_kbps(&self) -> u32 {
        self.ceiling_kbps
    }

    pub fn budget_kbps(&self) -> u32 {
        self.budget_kbps.round() as u32
    }

    pub fn congested(&self) -> bool {
        self.congested
    }

    /// Loss currently treated as the link's background loss (percent).
    pub fn random_loss_pct(&self) -> f32 {
        self.random_loss_pct
    }

    pub fn floor_kbps(&self) -> u32 {
        FLOOR_KBPS.min(self.ceiling_kbps)
    }

    /// The user's bitrate setting. Lowering it caps the budget at once;
    /// raising it lets the budget climb there.
    pub fn set_ceiling(&mut self, ceiling_kbps: u32) {
        let ceiling_kbps = ceiling_kbps.max(1);
        if ceiling_kbps == self.ceiling_kbps {
            return;
        }
        self.ceiling_kbps = ceiling_kbps;
        self.budget_kbps = self
            .budget_kbps
            .min(ceiling_kbps as f32)
            .max(self.floor_kbps() as f32);
    }

    /// A new viewer: start again (slow start) with nothing learned.
    pub fn reset(&mut self) {
        *self = Self::new(self.ceiling_kbps);
    }

    /// Still in slow start (no cut yet).
    pub fn in_slow_start(&self) -> bool {
        self.slow_start
    }

    /// Feed one sample; returns the new budget (kbit/s).
    pub fn on_sample(&mut self, s: Sample, now_ms: u64) -> u32 {
        let dt_ms = self
            .last_sample_ms
            .map(|t| now_ms.saturating_sub(t))
            .unwrap_or(500)
            .min(1_000);
        self.last_sample_ms = Some(now_ms);
        self.cut_now = false;

        let queue = s.queue;
        let queue_growing = match queue {
            Some(q) => {
                // Right after a cut the queue takes a while to drain (the
                // new rate is only a little below the link's): then only a
                // queue that is still rising counts.
                let draining = self
                    .last_cut_ms
                    .is_some_and(|t| now_ms.saturating_sub(t) < QUEUE_DRAIN_MS);
                (q.mean_ms() >= QUEUE_STANDING_MS && !draining)
                    || ((q.mean_ms() >= QUEUE_MS || q.max_ms() >= QUEUE_MAX_MS)
                        && q.trend_ms_per_s() >= QUEUE_TREND_MS_PER_S)
            }
            None => false,
        };
        // Loss with a flat queue is random (Wi-Fi retries, interference):
        // cutting would not reduce it.
        let queue_high = queue.is_some_and(|q| q.max_ms() >= QUEUE_LOSS_MS);
        // ...unless the budget is back where the link failed last time: a
        // shallow buffer (router, USB) drops before a queue shows.
        let near_bad = self
            .last_bad_kbps
            .is_some_and(|b| self.budget_kbps >= b * NEAR_BAD_SHARE);
        let delay_says_random = queue.is_some() && !queue_high && !self.queue_was_high && !near_bad;
        if queue.is_some() {
            self.queue_was_high = queue_high;
        }

        let loss = s
            .loss_pct
            .filter(|l| l.is_finite())
            .map(|l| l.clamp(0.0, 100.0));
        if let Some(l) = loss {
            self.learn_random_loss(l, now_ms);
        }
        let excess = loss
            .map(|l| (l - self.random_loss_pct).max(0.0))
            .unwrap_or(0.0);
        let excess = if delay_says_random && excess < SEVERE_EXCESS {
            0.0
        } else {
            excess
        };
        let raw = loss.unwrap_or(0.0);

        let lossy = excess >= 3.0 || queue_growing;
        self.lossy_streak = if lossy { self.lossy_streak + 1 } else { 0 };
        let heavy = excess >= 10.0;
        // A queue is direct evidence: one report is enough.
        self.congested = heavy || s.backlog_drops > 0 || queue_growing || self.lossy_streak >= 2;

        let floor = self.floor_kbps() as f32;
        let ceiling = self.ceiling_kbps as f32;
        if self.congested {
            let may_cut = self
                .last_cut_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= MIN_DECREASE_GAP_MS);
            if may_cut {
                let factor = if excess >= SEVERE_EXCESS {
                    0.6
                } else if heavy {
                    0.75
                } else {
                    0.85
                };
                let from_kbps = self.budget_kbps();
                let sent = s.sent_kbps.filter(|k| k.is_finite() && *k > 0.0);
                self.last_bad_kbps =
                    Some(sent.map_or(self.budget_kbps, |k| k.min(self.budget_kbps)));
                let mut target = self.budget_kbps * factor;
                let queue_cut = s.backlog_drops == 0 && !heavy && excess < 3.0;
                if queue_cut {
                    if let Some(sent) = sent {
                        target = target.min(sent * QUEUE_CUT_OF_SENT);
                    }
                }
                self.budget_kbps = target.max(floor);
                self.last_cut_ms = Some(now_ms);
                self.slow_start = false;
                let trigger = if s.backlog_drops > 0 {
                    CutTrigger::Backlog {
                        drops: s.backlog_drops,
                    }
                } else if heavy {
                    CutTrigger::HeavyLoss
                } else if excess >= 3.0 {
                    CutTrigger::Loss
                } else {
                    CutTrigger::Queue
                };
                self.last_cut = Some(Cut {
                    at_ms: now_ms,
                    trigger,
                    from_kbps,
                    to_kbps: self.budget_kbps(),
                    loss_pct: loss,
                    random_loss_pct: self.random_loss_pct,
                    queue,
                });
                self.cut_now = true;
                if loss.is_some() && self.probe.is_none() {
                    self.probe = Some(Probe {
                        loss_before: raw,
                        until_ms: now_ms + PROBE_MS,
                        sum: 0.0,
                        n: 0,
                    });
                }
            }
        } else if (excess < 1.5 || (self.slow_start && !lossy)) && !queue_growing {
            // Slow start keeps growing through loss below the cut threshold
            // (FEC covers it); background loss is only learned after a cut.
            let rested = self
                .last_cut_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= INCREASE_AFTER_CUT_MS);
            let used = s
                .sent_kbps
                .filter(|k| k.is_finite())
                .is_none_or(|k| k >= USED_SHARE * self.budget_kbps);
            if rested && used {
                let secs = dt_ms as f32 / 1000.0;
                let step = (ceiling * 0.04).max(MIN_STEP_KBPS);
                let step = match self.last_bad_kbps {
                    Some(b)
                        if self.budget_kbps >= b * NEAR_BAD_SHARE
                            && self.budget_kbps < b * PAST_BAD_SHARE =>
                    {
                        step * NEAR_BAD_STEP
                    }
                    _ => step,
                };
                self.budget_kbps = if self.slow_start {
                    self.budget_kbps * SLOW_START_PER_S.powf(secs)
                } else {
                    self.budget_kbps + step * secs
                }
                .min(ceiling);
            }
        }
        self.budget_kbps = self.budget_kbps.clamp(floor, ceiling);
        self.budget_kbps()
    }

    /// After a cut, watch the loss for a few seconds. If it stays (≥ 70 % of
    /// what it was), it did not come from our rate: remember it as the
    /// link's background loss. The estimate decays so it is re-learned.
    fn learn_random_loss(&mut self, loss: f32, now_ms: u64) {
        self.random_loss_pct *= 0.995;
        if let Some(p) = &mut self.probe {
            if now_ms < p.until_ms {
                p.sum += loss;
                p.n += 1;
                return;
            }
            let p = self.probe.take().expect("probe");
            if p.n > 0 {
                let after = p.sum / p.n as f32;
                if after >= 0.7 * p.loss_before {
                    self.random_loss_pct =
                        self.random_loss_pct.max(after * 0.9).min(MAX_RANDOM_LOSS);
                }
            }
        }
    }
}

/// Encoder target that keeps video + parity within `budget_kbps`, given the
/// FEC overhead (parity / data, e.g. 0.25) applied to delta frames.
pub fn encoder_kbps(budget_kbps: u32, fec_overhead: Option<f32>) -> u32 {
    let o = fec_overhead
        .filter(|o| o.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    ((budget_kbps as f32) / (1.0 + o)).round().max(100.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old behaviour: start at the ceiling (tests of the cut logic).
    fn at_ceiling(ceiling: u32) -> AdaptiveBitrate {
        AdaptiveBitrate::with_start(ceiling, ceiling)
    }

    fn loss(l: f32) -> Sample {
        Sample {
            loss_pct: Some(l),
            backlog_drops: 0,
            sent_kbps: None,
            queue: None,
        }
    }

    /// Feed `n` reports 500 ms apart starting at `*t`.
    fn feed(c: &mut AdaptiveBitrate, s: Sample, n: usize, t: &mut u64) -> u32 {
        let mut b = c.budget_kbps();
        for _ in 0..n {
            *t += 500;
            b = c.on_sample(s, *t);
        }
        b
    }

    #[test]
    fn clean_link_stays_at_the_ceiling() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut c, loss(0.0), 20, &mut t), 40_000);
        assert!(!c.congested());
    }

    #[test]
    fn sustained_loss_cuts_multiplicatively_once_per_second() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        // One moderate report is not yet congestion.
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 40_000);
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 34_000);
        assert!(c.congested());
        // 500 ms later: no second cut yet.
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 34_000);
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 28_900);
        // Heavy loss cuts at once and harder.
        let mut h = at_ceiling(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut h, loss(12.0), 1, &mut t), 30_000);
        let mut severe = at_ceiling(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut severe, loss(24.0), 1, &mut t), 24_000);
    }

    #[test]
    fn each_cut_records_its_trigger() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        feed(&mut c, loss(0.0), 2, &mut t);
        assert_eq!(c.last_cut(), None);
        feed(&mut c, loss(5.0), 2, &mut t);
        let cut = c.cut_by_last_sample().expect("loss cut");
        assert_eq!(cut.trigger, CutTrigger::Loss);
        assert_eq!((cut.from_kbps, cut.to_kbps), (40_000, 34_000));
        assert_eq!(cut.reason(), "loss 5.0 % (background 0.0 %)");
        assert_eq!(
            cut.to_string(),
            "cut 40.0 -> 34.0 Mbit/s: loss 5.0 % (background 0.0 %)"
        );
        // The next sample (no cut, gap < 1 s) keeps last_cut but clears the "now" flag.
        feed(&mut c, loss(5.0), 1, &mut t);
        assert!(c.cut_by_last_sample().is_none());
        assert_eq!(c.last_cut(), Some(cut));

        let mut b = at_ceiling(20_000);
        let drop = Sample {
            loss_pct: None,
            backlog_drops: 3,
            sent_kbps: None,
            queue: None,
        };
        feed(&mut b, drop, 1, &mut t);
        assert_eq!(
            b.last_cut().map(|c| c.trigger),
            Some(CutTrigger::Backlog { drops: 3 })
        );

        let mut h = at_ceiling(40_000);
        feed(&mut h, loss(12.0), 1, &mut t);
        assert_eq!(h.last_cut().map(|c| c.trigger), Some(CutTrigger::HeavyLoss));
    }

    #[test]
    fn slow_start_from_10_mbit_until_the_first_cut() {
        let mut c = AdaptiveBitrate::new(40_000);
        assert_eq!(c.budget_kbps(), START_KBPS);
        assert!(c.in_slow_start());
        let mut t = 0;
        // ×1.25 per second: 10 -> 15.6 Mbit/s after 2 s, the ceiling in ~6 s.
        assert_eq!(feed(&mut c, loss(0.0), 4, &mut t), 15_625);
        assert_eq!(feed(&mut c, loss(0.0), 8, &mut t), 38_147);
        assert_eq!(feed(&mut c, loss(0.0), 2, &mut t), 40_000);
        // A low ceiling is the start; the floor still holds.
        assert_eq!(AdaptiveBitrate::new(6_000).budget_kbps(), 6_000);
        assert_eq!(
            AdaptiveBitrate::with_start(40_000, 0).budget_kbps(),
            FLOOR_KBPS
        );
        // The first cut ends slow start: from then on additive.
        let mut s = AdaptiveBitrate::new(40_000);
        feed(&mut s, loss(12.0), 1, &mut t);
        assert!(!s.in_slow_start());
        assert_eq!(s.budget_kbps(), 7_500);
        assert_eq!(feed(&mut s, loss(0.0), 7, &mut t), 7_500, "4 s rest");
        assert_eq!(
            feed(&mut s, loss(0.0), 2, &mut t),
            9_100,
            "then +1.6 Mbit/s per s"
        );
        assert_eq!(
            feed(&mut s, loss(0.0), 1, &mut t),
            9_300,
            "a quarter of that from 90 % of the 10 Mbit/s that failed"
        );
        s.reset();
        assert!(s.in_slow_start());
        assert_eq!(s.budget_kbps(), START_KBPS);
    }

    #[test]
    fn budget_does_not_grow_while_unused() {
        let sent = |k: f32| Sample {
            loss_pct: Some(0.0),
            backlog_drops: 0,
            sent_kbps: Some(k),
            queue: None,
        };
        let mut t = 0;
        // Slow start: 10 Mbit/s budget, a static screen sends 2: no growth.
        let mut c = AdaptiveBitrate::new(40_000);
        assert_eq!(feed(&mut c, sent(2_000.0), 10, &mut t), START_KBPS);
        // Busy content fills it: growth resumes.
        assert!(feed(&mut c, sent(9_500.0), 2, &mut t) > START_KBPS);
        // After a cut, the additive climb also waits for use.
        let mut a = at_ceiling(40_000);
        feed(&mut a, loss(12.0), 1, &mut t);
        let cut = a.budget_kbps();
        assert_eq!(
            feed(&mut a, sent(12_000.0), 20, &mut t),
            cut,
            "12 of 30 Mbit/s used"
        );
        assert!(feed(&mut a, sent(28_000.0), 2, &mut t) > cut);
        // Garbage is "unknown", i.e. counts as used, not a panic.
        let mut g = AdaptiveBitrate::new(40_000);
        assert!(feed(&mut g, sent(f32::NAN), 2, &mut t) > START_KBPS);
    }

    #[test]
    fn floor_and_ceiling_hold() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut c, loss(30.0), 40, &mut t), FLOOR_KBPS);
        // A ceiling below the floor wins.
        let mut low = at_ceiling(2_000);
        assert_eq!(feed(&mut low, loss(30.0), 10, &mut t), 2_000);
        c.set_ceiling(2_500);
        assert_eq!(c.budget_kbps(), 2_500);
        c.set_ceiling(10_000);
        assert_eq!(
            c.budget_kbps(),
            FLOOR_KBPS,
            "raising the setting climbs from the floor, no jump"
        );
    }

    #[test]
    fn recovers_additively_after_a_pause() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        let cut = feed(&mut c, loss(12.0), 1, &mut t);
        assert_eq!(cut, 30_000);
        // Clean, but within 4 s of the cut: hold.
        assert_eq!(feed(&mut c, loss(0.0), 6, &mut t), 30_000);
        // From 4 s on: +4 % of the ceiling (1.6 Mbit) per second, i.e. 800 per report.
        let b = feed(&mut c, loss(0.0), 2, &mut t);
        assert_eq!(b, 30_800);
        assert_eq!(feed(&mut c, loss(0.0), 2, &mut t), 32_400);
        assert_eq!(feed(&mut c, loss(0.0), 40, &mut t), 40_000);
    }

    #[test]
    fn queue_growth_and_backlog_count_as_congestion() {
        let mut c = at_ceiling(20_000);
        let mut t = 0;
        feed(&mut c, queued(0.0, 2.0, 5.0, 0.0, None), 4, &mut t);
        assert_eq!(c.budget_kbps(), 20_000);
        assert_eq!(
            feed(&mut c, queued(0.0, 70.0, 90.0, 0.0, None), 1, &mut t),
            17_000
        );
        let mut b = at_ceiling(20_000);
        let drop = Sample {
            loss_pct: None,
            backlog_drops: 2,
            sent_kbps: None,
            queue: None,
        };
        assert_eq!(feed(&mut b, drop, 1, &mut t), 17_000);
        // Garbage input is ignored, not a panic.
        let junk = Sample {
            loss_pct: Some(f32::NAN),
            backlog_drops: 0,
            sent_kbps: None,
            queue: None,
        };
        feed(&mut b, junk, 3, &mut t);
    }

    #[test]
    fn climbs_slowly_near_the_budget_that_failed() {
        let mut c = at_ceiling(20_000);
        let mut t = 0;
        // Severe loss at 20 Mbit/s with 16 sent: the link failed at 16.
        let overrun = Sample {
            sent_kbps: Some(16_000.0),
            ..loss(25.0)
        };
        assert_eq!(feed(&mut c, overrun, 1, &mut t), 12_000);
        // 4 s rest, then +400 per report (4 % of 20 Mbit/s per second)...
        assert_eq!(feed(&mut c, loss(0.0), 7, &mut t), 12_000);
        assert_eq!(feed(&mut c, loss(0.0), 6, &mut t), 14_400);
        // ...a quarter of that from 14.4 (90 % of 16) to 18.4 (115 %)...
        assert_eq!(feed(&mut c, loss(0.0), 1, &mut t), 14_500);
        assert_eq!(feed(&mut c, loss(0.0), 39, &mut t), 18_400);
        // ...then full speed again: the link got better.
        assert_eq!(feed(&mut c, loss(0.0), 1, &mut t), 18_800);
    }

    #[test]
    fn near_the_failed_budget_loss_with_a_flat_queue_counts() {
        // A shallow buffer drops before a queue shows. Far below the
        // budget that failed, flat-queue loss is random...
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        feed(&mut c, loss(12.0), 1, &mut t);
        assert_eq!(c.budget_kbps(), 30_000);
        let flat = |l: f32| queued(l, 2.0, 10.0, 0.0, None);
        c.set_ceiling(20_000);
        feed(&mut c, flat(0.0), 10, &mut t);
        assert_eq!(c.budget_kbps(), 20_000);
        feed(&mut c, flat(5.0), 4, &mut t);
        assert_eq!(
            c.budget_kbps(),
            20_000,
            "20 is far below the 40 that failed"
        );
        // ...near it, the same loss is congestion.
        let mut n = at_ceiling(20_000);
        feed(&mut n, loss(12.0), 1, &mut t);
        feed(&mut n, flat(0.0), 30, &mut t);
        let before = n.budget_kbps();
        assert!(before >= 18_000, "{before}");
        feed(&mut n, flat(5.0), 2, &mut t);
        assert_eq!(n.last_cut().map(|c| c.trigger), Some(CutTrigger::Loss));
        assert!(n.budget_kbps() < before);
        // A new viewer forgets it.
        n.reset();
        feed(&mut n, flat(5.0), 4, &mut t);
        assert_eq!(n.last_cut(), None);
    }

    #[test]
    fn loss_that_ignores_the_rate_is_learned_as_random() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        // 6 % loss that never changes, whatever the rate (bad Wi-Fi, no congestion).
        let first = feed(&mut c, loss(6.0), 2, &mut t);
        assert_eq!(first, 34_000);
        feed(&mut c, loss(6.0), 30, &mut t);
        assert!(c.random_loss_pct() > 4.0, "{}", c.random_loss_pct());
        let settled = c.budget_kbps();
        assert!(
            settled > FLOOR_KBPS * 3,
            "kept cutting on random loss: {settled}"
        );
        // And it climbs back while the loss stays at that level.
        let later = feed(&mut c, loss(6.0), 40, &mut t);
        assert!(later > settled, "{settled} -> {later}");
        // Real congestion on top is still seen.
        assert!(feed(&mut c, loss(20.0), 2, &mut t) < later);
    }

    #[test]
    fn reset_and_encoder_share() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        feed(&mut c, loss(30.0), 10, &mut t);
        c.reset();
        assert_eq!(c.budget_kbps(), START_KBPS, "slow start again");
        assert_eq!(encoder_kbps(20_000, None), 20_000);
        assert_eq!(encoder_kbps(20_000, Some(0.25)), 16_000);
        assert_eq!(encoder_kbps(20_000, Some(f32::NAN)), 20_000);
        assert_eq!(
            encoder_kbps(20_000, Some(5.0)),
            10_000,
            "overhead capped at 100 %"
        );
        assert_eq!(encoder_kbps(0, None), 100);
    }

    fn queued(loss_pct: f32, mean_ms: f32, max_ms: f32, trend: f32, sent: Option<f32>) -> Sample {
        Sample {
            loss_pct: Some(loss_pct),
            backlog_drops: 0,
            sent_kbps: sent,
            queue: Some(QueueDelay {
                mean_us: (mean_ms * 1000.0) as u32,
                max_us: (max_ms * 1000.0) as u32,
                trend_us_per_s: (trend * 1000.0) as i32,
            }),
        }
    }

    #[test]
    fn a_building_queue_cuts_at_once_below_what_was_sent() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        feed(&mut c, queued(0.0, 3.0, 8.0, 0.0, None), 4, &mut t);
        assert_eq!(c.budget_kbps(), 40_000);
        // Small or falling queues are normal Wi-Fi jitter.
        feed(&mut c, queued(0.0, 20.0, 45.0, 80.0, None), 1, &mut t);
        feed(&mut c, queued(0.0, 10.0, 60.0, 20.0, None), 1, &mut t);
        feed(&mut c, queued(0.0, 30.0, 50.0, -50.0, None), 1, &mut t);
        assert_eq!(c.last_cut(), None);
        // One report of a rising queue: cut, to 90 % of the 30 Mbit/s sent.
        let b = feed(
            &mut c,
            queued(0.0, 30.0, 60.0, 100.0, Some(30_000.0)),
            1,
            &mut t,
        );
        assert_eq!(b, 27_000);
        let cut = c.last_cut().unwrap();
        assert_eq!(cut.trigger, CutTrigger::Queue);
        assert_eq!(cut.reason(), "queue 30 ms (max 60, +100 ms/s)");
        // While it drains (2 s), a standing queue does not cut again...
        feed(
            &mut c,
            queued(0.0, 70.0, 90.0, -20.0, Some(27_000.0)),
            3,
            &mut t,
        );
        assert_eq!(c.budget_kbps(), 27_000);
        // ...after that it does.
        feed(&mut c, queued(0.0, 70.0, 90.0, -20.0, None), 1, &mut t);
        assert_eq!(c.budget_kbps(), 22_950);
        // One frame that waited 82 ms in a rising queue is enough, even
        // with a small mean (the tablet, 2026-11-15: 24 ms, max 82, +193
        // ms/s, then 236 ms half a second later).
        let mut m = at_ceiling(40_000);
        feed(&mut m, queued(0.0, 24.0, 82.0, 193.0, None), 1, &mut t);
        assert_eq!(m.last_cut().map(|c| c.trigger), Some(CutTrigger::Queue));
    }

    #[test]
    fn loss_with_a_flat_queue_is_random() {
        let mut c = at_ceiling(40_000);
        let mut t = 0;
        // 8 % loss for 10 s, no frame ever waits: no cut, no congestion.
        assert_eq!(
            feed(&mut c, queued(8.0, 2.0, 10.0, 0.0, None), 20, &mut t),
            40_000
        );
        assert!(!c.congested());
        // The same loss with a queue behind it is congestion.
        feed(&mut c, queued(8.0, 20.0, 80.0, 0.0, None), 2, &mut t);
        assert_eq!(c.last_cut().map(|c| c.trigger), Some(CutTrigger::Loss));
        // Severe loss cuts even with a flat queue (it would also break FEC).
        let mut s = at_ceiling(40_000);
        feed(&mut s, queued(25.0, 2.0, 10.0, 0.0, None), 1, &mut t);
        assert_eq!(s.budget_kbps(), 24_000);
    }

    /// Closed-loop simulation without QUIC: encoder model -> FEC parity ->
    /// random loss -> bottleneck -> viewer report every 500 ms -> controller.
    struct Link {
        /// Bitrate the content needs (x264 sends less than the target on
        /// easy content, since there is no HRD filler).
        demand_kbps: u32,
        loss: f64,
        burst: f64,
        /// 0 = no bottleneck.
        bottleneck_kbps: u32,
        /// Every `busy_every_s` seconds the content needs `busy_kbps` for 2 s
        /// (a scene change after a quiet screen); 0 = never.
        busy_every_s: u64,
        busy_kbps: u32,
        /// The bottleneck's FIFO holds this many ms of data (Wi-Fi: the
        /// tablet showed ~290 ms before loss), so overload first shows as
        /// delay, then as loss.
        buffer_ms: u32,
    }

    /// A bottleneck with a FIFO buffer: packets wait behind the backlog and
    /// are dropped when the wait would exceed the buffer.
    struct Fifo {
        bytes_per_us: f64,
        buffer_us: f64,
        backlog: f64,
        last_us: u64,
    }

    impl Fifo {
        fn new(kbps: u32, buffer_ms: u32) -> Self {
            Self {
                bytes_per_us: kbps as f64 / 8_000.0,
                buffer_us: buffer_ms as f64 * 1000.0,
                backlog: 0.0,
                last_us: 0,
            }
        }

        /// The packet's queueing delay (µs), or `None` if it is dropped.
        fn offer(&mut self, len: usize, at_us: u64) -> Option<u64> {
            let dt = at_us.saturating_sub(self.last_us) as f64;
            self.last_us = at_us;
            self.backlog = (self.backlog - dt * self.bytes_per_us).max(0.0);
            let wait = self.backlog / self.bytes_per_us;
            if wait > self.buffer_us {
                return None;
            }
            self.backlog += len as f64;
            Some(wait as u64)
        }
    }

    struct SimResult {
        cuts: Vec<Cut>,
        /// Budget at the end, and the lowest after the first 10 s.
        final_kbps: u32,
        min_kbps: u32,
        /// Video bytes that arrived (data packets), as kbit/s over the last 20 s.
        delivered_kbps: f32,
        /// Packets lost over the whole run (percent of those sent).
        lost_pct: f32,
        /// Queueing delay of the frames' first packets after the first 10 s
        /// (ms): mean and 95th percentile.
        queue_mean_ms: f32,
        queue_p95_ms: f32,
    }

    fn simulate(link: Link, ceiling_kbps: u32, secs: u64) -> SimResult {
        use crate::fec::{FecMode, LossSim};
        const PKT: usize = 1200;
        let mut c = AdaptiveBitrate::new(ceiling_kbps);
        let mut lossy = LossSim::new(link.loss, link.burst, 0x5eed);
        let mut fifo =
            (link.bottleneck_kbps > 0).then(|| Fifo::new(link.bottleneck_kbps, link.buffer_ms));
        let mut meter = crate::delay::DelayMeter::default();
        let mut queue_samples = crate::stats::Series::default();
        let (mut smoothed_loss, mut reports) = (0.0f32, 0u32);
        let (mut expected, mut received) = (0u64, 0u64);
        let (mut cuts, mut min_kbps, mut late_bytes) = (Vec::new(), u32::MAX, 0usize);
        let (mut total_sent, mut total_lost, mut window_sent) = (0u64, 0u64, 0u64);
        let frame_us = 1_000_000 / 60;
        for frame in 0..secs * 60 {
            let t_us = frame * frame_us;
            let overhead = FecMode::Auto
                .overhead(smoothed_loss, 0.0, false)
                .unwrap_or(0.0);
            let sec = t_us / 1_000_000;
            let busy = link.busy_every_s > 0 && sec % link.busy_every_s >= link.busy_every_s - 2;
            let demand = if busy {
                link.busy_kbps
            } else {
                link.demand_kbps
            };
            let target = encoder_kbps(c.budget_kbps(), Some(overhead)).min(demand);
            let data = (target as usize * 1000 / 8 / 60).div_ceil(PKT).max(1);
            let parity = (data as f32 * overhead).ceil() as usize;
            let mut frame_seen = false;
            for k in 0..data + parity {
                // Paced at 500 Mbit/s: 1200 bytes every ~19 us.
                let at = t_us + k as u64 * 19;
                expected += 1;
                (total_sent, window_sent) = (total_sent + 1, window_sent + 1);
                if lossy.drop_next() {
                    continue;
                }
                let wait = match fifo.as_mut() {
                    Some(f) => match f.offer(PKT, at) {
                        Some(w) => w,
                        None => continue,
                    },
                    None => 0,
                };
                // The first packet of the frame that arrives is what the
                // viewer measures (sent_us = frame start, +3 ms path).
                meter.on_packet(frame as u32, t_us, at + wait + 3_000);
                if !std::mem::replace(&mut frame_seen, true) && t_us >= 10_000_000 {
                    queue_samples.push(wait as f64 / 1000.0);
                }
                received += 1;
                if k < data && t_us >= (secs - 20) * 1_000_000 {
                    late_bytes += PKT;
                }
            }
            if (frame + 1) % 30 == 0 {
                total_lost += expected - received;
                let l = 100.0 * (expected - received) as f32 / expected.max(1) as f32;
                // 30 frames = 0.5 s.
                let sent_kbps = (window_sent * PKT as u64 * 8) as f32 / 1000.0 / 0.5;
                window_sent = 0;
                let w = if l > smoothed_loss { 0.5 } else { 0.15 };
                smoothed_loss = if reports == 0 {
                    l
                } else {
                    smoothed_loss + w * (l - smoothed_loss)
                };
                reports += 1;
                let now_ms = (t_us + frame_us) / 1000;
                c.on_sample(
                    Sample {
                        loss_pct: Some(l),
                        backlog_drops: 0,
                        sent_kbps: Some(sent_kbps),
                        queue: meter.take(),
                    },
                    now_ms,
                );
                cuts.extend(c.cut_by_last_sample());
                if now_ms >= 10_000 {
                    min_kbps = min_kbps.min(c.budget_kbps());
                }
                (expected, received) = (0, 0);
            }
        }
        SimResult {
            cuts,
            final_kbps: c.budget_kbps(),
            min_kbps,
            delivered_kbps: late_bytes as f32 * 8.0 / 1000.0 / 20.0,
            lost_pct: 100.0 * total_lost as f32 / total_sent.max(1) as f32,
            queue_mean_ms: queue_samples.summary().mean as f32,
            queue_p95_ms: queue_samples.summary().p95 as f32,
        }
    }

    fn print(name: &str, r: &SimResult) {
        eprintln!(
            "{name}: {} cuts, final {} kbit/s, min after 10 s {}, delivered video {:.0} kbit/s, lost {:.2} %, queue mean {:.1} p95 {:.1} ms",
            r.cuts.len(),
            r.final_kbps,
            r.min_kbps,
            r.delivered_kbps,
            r.lost_pct,
            r.queue_mean_ms,
            r.queue_p95_ms
        );
        for cut in r.cuts.iter().take(8) {
            eprintln!("  {:>6} ms {cut}", cut.at_ms);
        }
    }

    const LAN: Link = Link {
        demand_kbps: 60_000,
        loss: 0.0,
        burst: 1.0,
        bottleneck_kbps: 0,
        busy_every_s: 0,
        busy_kbps: 0,
        buffer_ms: 150,
    };

    /// The tablet link of 2026-11-15 (second run): ~16 Mbit/s, deep buffer.
    const WIFI: Link = Link {
        demand_kbps: 60_000,
        bottleneck_kbps: 16_000,
        buffer_ms: 300,
        ..LAN
    };

    #[test]
    fn sim_random_loss_without_a_bottleneck_keeps_the_ceiling() {
        // No bottleneck, so the queue stays flat: random loss is FEC's job
        // and the rate never drops (with the RTT rule, 5 % cost 3 cuts).
        for loss in [0.0, 0.02, 0.05] {
            let r = simulate(Link { loss, ..LAN }, 40_000, 60);
            print(&format!("no limit, {:.0} % random", loss * 100.0), &r);
            assert!(r.cuts.is_empty(), "{:?}", r.cuts);
            assert!(r.final_kbps >= 39_000, "{}", r.final_kbps);
        }
    }

    #[test]
    fn sim_easy_content_is_not_mistaken_for_a_cut() {
        // The encoder sends 15 of 40 Mbit/s (test pattern): no cut, and
        // the budget stops ~1.25x above what is used (wire rate with parity)
        // instead of climbing to the ceiling.
        let r = simulate(
            Link {
                demand_kbps: 15_000,
                loss: 0.02,
                ..LAN
            },
            40_000,
            60,
        );
        print("easy content, 2 % random", &r);
        assert!(r.cuts.is_empty());
        assert!(
            (20_000..=28_000).contains(&r.final_kbps),
            "{}",
            r.final_kbps
        );
        assert!(
            r.delivered_kbps > 14_500.0,
            "content fully sent: {}",
            r.delivered_kbps
        );
    }

    #[test]
    fn sim_quiet_screen_with_bursts_does_not_overrun_the_link() {
        // The tablet case (2026-11-15): ~12 Mbit/s content, 2 s bursts of
        // 24 Mbit/s every 10 s, a ~20 Mbit/s Wi-Fi link.
        let link = Link {
            demand_kbps: 12_000,
            busy_every_s: 10,
            busy_kbps: 24_000,
            bottleneck_kbps: 20_000,
            ..LAN
        };
        let r = simulate(link, 40_000, 90);
        print("12 Mbit/s content, 24 Mbit/s bursts, 20 Mbit/s link", &r);
        // The budget stays near what is used, so most bursts are capped by
        // the encoder instead of overrunning the link, and the queue cuts
        // the rest before packets are lost. Measured with a 150 ms FIFO:
        // 6 queue cuts, 0.00 % lost (with a drop-only bottleneck and the
        // loss/RTT rules: 4 cuts, 0.74 %; the controller before slow start
        // and "grow only while used": 10 cuts, 2.40 %).
        assert!(r.cuts.len() <= 6, "{} cuts", r.cuts.len());
        assert!(r.lost_pct < 0.1, "{:.2} % lost", r.lost_pct);
    }

    #[test]
    fn sim_bottleneck_converges_below_the_capacity() {
        for (cap, loss) in [(20_000, 0.0), (20_000, 0.02), (8_000, 0.02)] {
            let r = simulate(
                Link {
                    bottleneck_kbps: cap,
                    loss,
                    ..LAN
                },
                40_000,
                90,
            );
            print(
                &format!(
                    "{} Mbit/s bottleneck, {:.0} % random",
                    cap / 1000,
                    loss * 100.0
                ),
                &r,
            );
            assert!(!r.cuts.is_empty());
            // The sawtooth peaks only a little over the capacity: at most
            // 15 %, or one second of the additive step (1.6 Mbit/s here).
            let peak = r
                .cuts
                .iter()
                .skip(1)
                .map(|c| c.from_kbps)
                .max()
                .unwrap_or(0);
            assert!(
                peak <= (cap * 115 / 100).max(cap + 1_600),
                "peak {peak} over {cap}"
            );
            assert!(
                r.delivered_kbps >= cap as f32 * 0.4,
                "{} of {cap}",
                r.delivered_kbps
            );
        }
    }

    #[test]
    fn sim_queue_finds_the_bottleneck_before_loss() {
        // 16 Mbit/s with a deep Wi-Fi buffer (the tablet showed ~290 ms):
        // every cut comes from the queue, before a packet is lost.
        // Measured: 16 cuts, 0.00 % lost, queue p95 70.5 ms, 13.8 Mbit/s
        // video delivered (the RTT rule on the same link: 19 cuts, p95
        // 129 ms, 13.0 Mbit/s).
        let r = simulate(WIFI, 40_000, 90);
        print("16 Mbit/s, 300 ms buffer", &r);
        assert!(
            r.cuts.iter().all(|c| c.trigger == CutTrigger::Queue),
            "{:?}",
            r.cuts
        );
        assert!(r.lost_pct < 0.2, "{:.2} % lost", r.lost_pct);
        assert!(r.queue_p95_ms < 100.0, "{}", r.queue_p95_ms);
        assert!(r.delivered_kbps >= 16_000.0 * 0.8, "{}", r.delivered_kbps);
    }

    #[test]
    fn sim_shallow_buffer_is_found_without_heavy_loss() {
        // A bottleneck that drops before a queue of QUEUE_LOSS_MS builds
        // (a router or USB link) looks like random loss. Near the budget
        // that failed last time, loss counts anyway and the climb is slow.
        // Measured: 50 ms buffer 0.72 % lost, 13.3 Mbit/s video; 20 ms
        // 1.19 %, 13.0 (before, loss counted only once heavy: 4.92 % /
        // 11.7 and 5.53 % / 11.3, each overrun ~23 % loss in one report).
        for buffer_ms in [50, 20] {
            let r = simulate(Link { buffer_ms, ..WIFI }, 40_000, 90);
            print(&format!("16 Mbit/s, {buffer_ms} ms buffer"), &r);
            assert!(r.lost_pct < 2.0, "{:.2} % lost", r.lost_pct);
            assert!(r.delivered_kbps >= 16_000.0 * 0.75, "{}", r.delivered_kbps);
        }
    }
}
