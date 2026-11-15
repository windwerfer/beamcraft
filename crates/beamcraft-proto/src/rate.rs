//! Adaptive bitrate for the presenter: an AIMD controller driven by the
//! viewer's `link_report` (packet loss) and the QUIC RTT.
//!
//! The controller owns the **total budget** on the wire (video + FEC
//! parity). [`encoder_kbps`] turns it into the encoder target by leaving
//! room for the parity the FEC setting will add, so FEC never pushes the
//! stream above the budget (on congested Wi-Fi, extra parity only made the
//! loss worse).
//!
//! * Congestion: loss clearly above what the link loses anyway, sustained
//!   for two reports (or one heavy report), a queue building up (RTT well
//!   above the lowest seen), or the presenter dropping frames for backlog.
//!   Then the budget is cut multiplicatively (×0.85, ×0.75 heavy, ×0.6
//!   severe), at most once per second, never below the floor.
//! * Clean (little loss, no queue): the budget climbs additively by 4 % of
//!   the ceiling per second, starting 4 s after the last cut.
//! * Loss that does not go away when the rate is cut is learned as the
//!   link's random loss (Wi-Fi retries, interference); FEC handles it, and
//!   it does not keep pushing the rate down.
//!
//! Time comes in as milliseconds so the logic is testable without a clock.

/// Lowest budget the controller goes to (kbit/s), or the ceiling if lower.
pub const FLOOR_KBPS: u32 = 3_000;
/// Smallest additive step per second (kbit/s).
const MIN_STEP_KBPS: f32 = 250.0;
const MIN_DECREASE_GAP_MS: u64 = 1_000;
const INCREASE_AFTER_CUT_MS: u64 = 4_000;
/// How long after a cut the loss is watched to see whether it followed the rate.
const PROBE_MS: u64 = 3_000;
const MAX_RANDOM_LOSS: f32 = 15.0;

/// One feedback sample. `loss_pct` is `None` when only the presenter's own
/// view is known (send window without a viewer report).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    pub loss_pct: Option<f32>,
    pub rtt_ms: Option<f32>,
    /// Delta frames the presenter dropped because the send queue was full.
    pub backlog_drops: u32,
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
    /// RTT well above the lowest seen (a queue is building up).
    Rtt,
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
    pub rtt_ms: Option<f32>,
    pub min_rtt_ms: Option<f32>,
}

impl Cut {
    /// Short reason, e.g. `loss 5.2 % (background 1.8 %)` or `rtt 48 ms (min 3 ms)`.
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
            CutTrigger::Rtt => format!(
                "rtt {:.0} ms (min {:.0} ms)",
                self.rtt_ms.unwrap_or(0.0),
                self.min_rtt_ms.unwrap_or(0.0)
            ),
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
    min_rtt_ms: Option<f32>,
    last_sample_ms: Option<u64>,
    last_cut_ms: Option<u64>,
    lossy_streak: u32,
    congested: bool,
    /// Loss the link shows regardless of the rate (learned, decays).
    random_loss_pct: f32,
    /// Loss when the last cut happened, and the samples since (probe).
    probe: Option<Probe>,
    /// The most recent cut, kept until the next one.
    last_cut: Option<Cut>,
    /// Set by `on_sample` when that sample cut the budget.
    cut_now: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Probe {
    loss_before: f32,
    until_ms: u64,
    sum: f32,
    n: u32,
}

impl AdaptiveBitrate {
    pub fn new(ceiling_kbps: u32) -> Self {
        let ceiling_kbps = ceiling_kbps.max(1);
        Self {
            ceiling_kbps,
            budget_kbps: ceiling_kbps as f32,
            min_rtt_ms: None,
            last_sample_ms: None,
            last_cut_ms: None,
            lossy_streak: 0,
            congested: false,
            random_loss_pct: 0.0,
            probe: None,
            last_cut: None,
            cut_now: false,
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

    /// A new viewer: start again from the ceiling with nothing learned.
    pub fn reset(&mut self) {
        *self = Self::new(self.ceiling_kbps);
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
        let rtt_in = s.rtt_ms.filter(|r| r.is_finite() && *r > 0.0);

        let queue_growing = match rtt_in {
            Some(rtt) => {
                let min = self.min_rtt_ms.get_or_insert(rtt);
                // Follow route changes slowly upwards, instantly downwards.
                *min = if rtt < *min {
                    rtt
                } else {
                    *min + 0.005 * (rtt - *min)
                };
                rtt > *min * 1.5 + 25.0
            }
            None => false,
        };

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
        let raw = loss.unwrap_or(0.0);

        let lossy = excess >= 3.0 || queue_growing;
        self.lossy_streak = if lossy { self.lossy_streak + 1 } else { 0 };
        let heavy = excess >= 10.0;
        self.congested = heavy || s.backlog_drops > 0 || self.lossy_streak >= 2;

        let floor = self.floor_kbps() as f32;
        let ceiling = self.ceiling_kbps as f32;
        if self.congested {
            let may_cut = self
                .last_cut_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= MIN_DECREASE_GAP_MS);
            if may_cut {
                let factor = if excess >= 20.0 {
                    0.6
                } else if heavy {
                    0.75
                } else {
                    0.85
                };
                let from_kbps = self.budget_kbps();
                self.budget_kbps = (self.budget_kbps * factor).max(floor);
                self.last_cut_ms = Some(now_ms);
                let trigger = if s.backlog_drops > 0 {
                    CutTrigger::Backlog {
                        drops: s.backlog_drops,
                    }
                } else if heavy {
                    CutTrigger::HeavyLoss
                } else if excess >= 3.0 {
                    CutTrigger::Loss
                } else {
                    CutTrigger::Rtt
                };
                self.last_cut = Some(Cut {
                    at_ms: now_ms,
                    trigger,
                    from_kbps,
                    to_kbps: self.budget_kbps(),
                    loss_pct: loss,
                    random_loss_pct: self.random_loss_pct,
                    rtt_ms: rtt_in,
                    min_rtt_ms: self.min_rtt_ms,
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
        } else if excess < 1.5 && !queue_growing {
            let rested = self
                .last_cut_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= INCREASE_AFTER_CUT_MS);
            if rested {
                let step = (ceiling * 0.04).max(MIN_STEP_KBPS) * dt_ms as f32 / 1000.0;
                self.budget_kbps = (self.budget_kbps + step).min(ceiling);
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

    fn loss(l: f32) -> Sample {
        Sample {
            loss_pct: Some(l),
            rtt_ms: Some(5.0),
            backlog_drops: 0,
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
        let mut c = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut c, loss(0.0), 20, &mut t), 40_000);
        assert!(!c.congested());
    }

    #[test]
    fn sustained_loss_cuts_multiplicatively_once_per_second() {
        let mut c = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        // One moderate report is not yet congestion.
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 40_000);
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 34_000);
        assert!(c.congested());
        // 500 ms later: no second cut yet.
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 34_000);
        assert_eq!(feed(&mut c, loss(5.0), 1, &mut t), 28_900);
        // Heavy loss cuts at once and harder.
        let mut h = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut h, loss(12.0), 1, &mut t), 30_000);
        let mut severe = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut severe, loss(24.0), 1, &mut t), 24_000);
    }

    #[test]
    fn each_cut_records_its_trigger() {
        let mut c = AdaptiveBitrate::new(40_000);
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

        let mut r = AdaptiveBitrate::new(20_000);
        let rtt = |ms| Sample {
            loss_pct: Some(0.0),
            rtt_ms: Some(ms),
            backlog_drops: 0,
        };
        feed(&mut r, rtt(4.0), 4, &mut t);
        feed(&mut r, rtt(80.0), 2, &mut t);
        let cut = r.last_cut().expect("rtt cut");
        assert_eq!(cut.trigger, CutTrigger::Rtt);
        assert!(
            cut.reason().starts_with("rtt 80 ms (min "),
            "{}",
            cut.reason()
        );

        let mut b = AdaptiveBitrate::new(20_000);
        let drop = Sample {
            loss_pct: None,
            rtt_ms: None,
            backlog_drops: 3,
        };
        feed(&mut b, drop, 1, &mut t);
        assert_eq!(
            b.last_cut().map(|c| c.trigger),
            Some(CutTrigger::Backlog { drops: 3 })
        );

        let mut h = AdaptiveBitrate::new(40_000);
        feed(&mut h, loss(12.0), 1, &mut t);
        assert_eq!(h.last_cut().map(|c| c.trigger), Some(CutTrigger::HeavyLoss));
    }

    #[test]
    fn floor_and_ceiling_hold() {
        let mut c = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        assert_eq!(feed(&mut c, loss(30.0), 40, &mut t), FLOOR_KBPS);
        // A ceiling below the floor wins.
        let mut low = AdaptiveBitrate::new(2_000);
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
        let mut c = AdaptiveBitrate::new(40_000);
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
        let mut c = AdaptiveBitrate::new(20_000);
        let mut t = 0;
        let rtt = |ms| Sample {
            loss_pct: Some(0.0),
            rtt_ms: Some(ms),
            backlog_drops: 0,
        };
        feed(&mut c, rtt(4.0), 4, &mut t);
        assert_eq!(c.budget_kbps(), 20_000);
        assert_eq!(feed(&mut c, rtt(80.0), 2, &mut t), 17_000);
        let mut b = AdaptiveBitrate::new(20_000);
        let drop = Sample {
            loss_pct: None,
            rtt_ms: None,
            backlog_drops: 2,
        };
        assert_eq!(feed(&mut b, drop, 1, &mut t), 17_000);
        // Garbage input is ignored, not a panic.
        let junk = Sample {
            loss_pct: Some(f32::NAN),
            rtt_ms: Some(f32::INFINITY),
            backlog_drops: 0,
        };
        feed(&mut b, junk, 3, &mut t);
    }

    #[test]
    fn loss_that_ignores_the_rate_is_learned_as_random() {
        let mut c = AdaptiveBitrate::new(40_000);
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
        let mut c = AdaptiveBitrate::new(40_000);
        let mut t = 0;
        feed(&mut c, loss(30.0), 10, &mut t);
        c.reset();
        assert_eq!(c.budget_kbps(), 40_000);
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

    /// Closed-loop simulation without QUIC: encoder model -> FEC parity ->
    /// random loss -> bottleneck -> viewer report every 500 ms -> controller.
    struct Link {
        /// Bitrate the content needs (x264 sends less than the target on
        /// easy content, since there is no HRD filler).
        demand_kbps: u32,
        loss: f64,
        burst: f64,
        bottleneck_kbps: u32,
        rtt_ms: f32,
    }

    struct SimResult {
        cuts: Vec<Cut>,
        /// Budget at the end, and the lowest after the first 10 s.
        final_kbps: u32,
        min_kbps: u32,
        /// Video bytes that arrived (data packets), as kbit/s over the last 20 s.
        delivered_kbps: f32,
    }

    fn simulate(link: Link, ceiling_kbps: u32, secs: u64) -> SimResult {
        use crate::fec::{BandwidthSim, FecMode, LossSim};
        const PKT: usize = 1200;
        let mut c = AdaptiveBitrate::new(ceiling_kbps);
        let mut lossy = LossSim::new(link.loss, link.burst, 0x5eed);
        let mut bw = BandwidthSim::new(link.bottleneck_kbps, 150);
        let (mut smoothed_loss, mut reports) = (0.0f32, 0u32);
        let (mut expected, mut received) = (0u64, 0u64);
        let (mut cuts, mut min_kbps, mut late_bytes) = (Vec::new(), u32::MAX, 0usize);
        let frame_us = 1_000_000 / 60;
        for frame in 0..secs * 60 {
            let t_us = frame * frame_us;
            let overhead = FecMode::Auto
                .overhead(smoothed_loss, 0.0, false)
                .unwrap_or(0.0);
            let target = encoder_kbps(c.budget_kbps(), Some(overhead)).min(link.demand_kbps);
            let data = (target as usize * 1000 / 8 / 60).div_ceil(PKT).max(1);
            let parity = (data as f32 * overhead).ceil() as usize;
            for k in 0..data + parity {
                // Paced at 500 Mbit/s: 1200 bytes every ~19 us.
                let at = t_us + k as u64 * 19;
                expected += 1;
                if lossy.drop_next() || bw.as_mut().is_some_and(|b| !b.admit(PKT, at)) {
                    continue;
                }
                received += 1;
                if k < data && t_us >= (secs - 20) * 1_000_000 {
                    late_bytes += PKT;
                }
            }
            if (frame + 1) % 30 == 0 {
                let l = 100.0 * (expected - received) as f32 / expected.max(1) as f32;
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
                        rtt_ms: Some(link.rtt_ms),
                        backlog_drops: 0,
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
        }
    }

    fn print(name: &str, r: &SimResult) {
        eprintln!(
            "{name}: {} cuts, final {} kbit/s, min after 10 s {}, delivered video {:.0} kbit/s",
            r.cuts.len(),
            r.final_kbps,
            r.min_kbps,
            r.delivered_kbps
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
        rtt_ms: 3.0,
    };

    #[test]
    fn sim_random_loss_without_a_bottleneck_keeps_the_ceiling() {
        // 2 % is below the 3-point excess rule: never a cut. 5 % cuts until
        // the probe has learned it as background loss (today 3 cuts in the
        // first 3 s, since cuts go on while the probe watches), then climbs back.
        for (loss, max_cuts) in [(0.0, 0), (0.02, 0), (0.05, 3)] {
            let r = simulate(Link { loss, ..LAN }, 40_000, 60);
            print(&format!("no limit, {:.0} % random", loss * 100.0), &r);
            assert!(r.cuts.len() <= max_cuts, "random loss kept cutting");
            assert!(r.cuts.iter().all(|c| c.at_ms < 5_000), "cut after learning");
            assert!(r.final_kbps >= 36_000, "{}", r.final_kbps);
        }
    }

    #[test]
    fn sim_easy_content_is_not_mistaken_for_a_cut() {
        // The encoder sends 15 of 40 Mbit/s (test pattern): no cut, full budget.
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
        assert_eq!(r.final_kbps, 40_000);
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
            assert!(r.cuts.iter().all(|c| !matches!(c.trigger, CutTrigger::Rtt)));
            // Settles under the capacity and uses a fair share of it.
            assert!(r.final_kbps <= cap * 11 / 10, "{} over {cap}", r.final_kbps);
            assert!(
                r.delivered_kbps >= cap as f32 * 0.4,
                "{} of {cap}",
                r.delivered_kbps
            );
        }
    }
}
