//! The "Bottleneck" line on the viewer HUD: one verdict per second-ish window from the stats the
//! viewer already has (loss, queueing delay, decode time, source fps, encoder).
//!
//! Coarse on purpose: a stage and a rating (good / OK / weak), no percentages. Thresholds are
//! documented in `docs/agents/architecture.md` (Bottleneck line) and tested below. A verdict
//! only changes after [`HOLD_WINDOWS`] equal windows in a row, so the line does not flicker.

use std::fmt;

/// Windows (about one second each) a new verdict must hold before it replaces the shown one.
pub const HOLD_WINDOWS: u32 = 3;

/// Loss in percent: OK from here, weak from `LOSS_WEAK`.
const LOSS_OK: f32 = 1.0;
const LOSS_WEAK: f32 = 5.0;
/// Mean one-way queueing delay in ms.
const QUEUE_OK_MS: f32 = 15.0;
const QUEUE_WEAK_MS: f32 = 40.0;
/// Decode time as a share of the frame interval.
const DECODE_OK_SHARE: f64 = 0.5;
const DECODE_WEAK_SHARE: f64 = 0.8;
/// The source delivers less than this share of the requested fps while the encoder is software.
const FPS_SHARE: f64 = 0.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    None,
    Network,
    Viewer,
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rating {
    Good,
    Ok,
    Weak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub stage: Stage,
    pub rating: Rating,
}

/// What one window measured.
#[derive(Debug, Clone, Copy)]
pub struct Sample<'a> {
    pub loss_pct: f32,
    /// Mean queueing delay, if the viewer measured one.
    pub queue_ms: Option<f32>,
    pub decode_ms: f64,
    pub software_decoder: bool,
    pub source_fps: f64,
    /// The fps the presenter was asked for (`stream_info.fps`), 0 if unknown.
    pub requested_fps: u32,
    pub encoder: &'a str,
}

fn is_software_encoder(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("x264") || n.contains("software") || n.contains("openh264")
}

/// The verdict of one window, without hysteresis.
pub fn classify(s: &Sample) -> Verdict {
    let v = |stage, rating| Verdict { stage, rating };
    let queue = s.queue_ms.unwrap_or(0.0);
    if s.loss_pct >= LOSS_WEAK || queue >= QUEUE_WEAK_MS {
        return v(Stage::Network, Rating::Weak);
    }
    let budget_ms = if s.requested_fps > 0 {
        1000.0 / f64::from(s.requested_fps)
    } else {
        1000.0 / 60.0
    };
    if s.decode_ms >= budget_ms * DECODE_WEAK_SHARE {
        return v(Stage::Viewer, Rating::Weak);
    }
    let slow_source = s.requested_fps > 0
        && s.source_fps > 0.0
        && s.source_fps < f64::from(s.requested_fps) * FPS_SHARE;
    if slow_source && is_software_encoder(s.encoder) {
        return v(Stage::Driver, Rating::Weak);
    }
    if s.loss_pct >= LOSS_OK || queue >= QUEUE_OK_MS {
        return v(Stage::Network, Rating::Ok);
    }
    if s.decode_ms >= budget_ms * DECODE_OK_SHARE {
        return v(Stage::Viewer, Rating::Ok);
    }
    v(Stage::None, Rating::Good)
}

impl fmt::Display for Verdict {
    /// `network - weak: 8 % loss...` is built by [`Bottleneck::line`]; this is the short form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            Stage::None => "none",
            Stage::Network => "network",
            Stage::Viewer => "viewer",
            Stage::Driver => "driver",
        };
        let rating = match self.rating {
            Rating::Good => "good",
            Rating::Ok => "OK",
            Rating::Weak => "weak",
        };
        write!(f, "{stage} - {rating}")
    }
}

/// The classifier with hysteresis; feed it one [`Sample`] per window.
#[derive(Debug, Clone)]
pub struct Bottleneck {
    shown: Verdict,
    candidate: Verdict,
    streak: u32,
    hint: String,
}

impl Default for Bottleneck {
    fn default() -> Self {
        let good = Verdict {
            stage: Stage::None,
            rating: Rating::Good,
        };
        Self {
            shown: good,
            candidate: good,
            streak: 0,
            hint: hint(good, &no_sample()),
        }
    }
}

fn no_sample() -> Sample<'static> {
    Sample {
        loss_pct: 0.0,
        queue_ms: None,
        decode_ms: 0.0,
        software_decoder: false,
        source_fps: 0.0,
        requested_fps: 0,
        encoder: "",
    }
}

fn hint(v: Verdict, s: &Sample) -> String {
    match (v.stage, v.rating) {
        (Stage::None, _) => "all stages keep up".to_owned(),
        (Stage::Network, Rating::Weak) => format!(
            "{:.0} % loss, {:.0} ms queue, likely Wi-Fi; cable or 5 GHz helps",
            s.loss_pct,
            s.queue_ms.unwrap_or(0.0)
        ),
        (Stage::Network, _) => format!(
            "{:.1} % loss, {:.0} ms queue, link is near its limit",
            s.loss_pct,
            s.queue_ms.unwrap_or(0.0)
        ),
        (Stage::Viewer, _) => format!(
            "{} decode {:.1} ms{}",
            if s.software_decoder {
                "software"
            } else {
                "hardware"
            },
            s.decode_ms,
            if s.software_decoder {
                "; a hardware decoder or lower fps helps"
            } else {
                ""
            }
        ),
        (Stage::Driver, _) => format!(
            "CPU encoding ({}), source {:.0} of {} fps; no hardware H.264 encoder",
            s.encoder, s.source_fps, s.requested_fps
        ),
    }
}

impl Bottleneck {
    /// Take one window; the shown verdict changes only after [`HOLD_WINDOWS`] equal ones.
    pub fn update(&mut self, s: &Sample) {
        let v = classify(s);
        if v == self.shown {
            self.candidate = v;
            self.streak = 0;
            // Keep the numbers in the text fresh without changing the verdict.
            self.hint = hint(v, s);
            return;
        }
        if v == self.candidate {
            self.streak += 1;
        } else {
            self.candidate = v;
            self.streak = 1;
        }
        if self.streak >= HOLD_WINDOWS {
            self.shown = v;
            self.streak = 0;
            self.hint = hint(v, s);
        }
    }

    #[cfg(test)]
    pub fn verdict(&self) -> Verdict {
        self.shown
    }

    /// `Bottleneck: network - weak: 8 % loss, ...`
    pub fn line(&self) -> String {
        format!("Bottleneck: {}: {}", self.shown, self.hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Sample<'static> {
        Sample {
            loss_pct: 0.0,
            queue_ms: Some(2.0),
            decode_ms: 3.0,
            software_decoder: false,
            source_fps: 60.0,
            requested_fps: 60,
            encoder: "h264_vaapi",
        }
    }

    fn stage_rating(s: &Sample) -> (Stage, Rating) {
        let v = classify(s);
        (v.stage, v.rating)
    }

    #[test]
    fn healthy_link_is_good() {
        assert_eq!(stage_rating(&good()), (Stage::None, Rating::Good));
    }

    #[test]
    fn loss_and_queue_are_network() {
        let s = Sample {
            loss_pct: 22.0,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Network, Rating::Weak));
        let s = Sample {
            queue_ms: Some(60.0),
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Network, Rating::Weak));
        let s = Sample {
            loss_pct: 2.0,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Network, Rating::Ok));
        let s = Sample {
            queue_ms: Some(20.0),
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Network, Rating::Ok));
    }

    #[test]
    fn slow_decode_is_viewer() {
        let s = Sample {
            decode_ms: 15.0,
            software_decoder: true,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Viewer, Rating::Weak));
        let s = Sample {
            decode_ms: 9.0,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Viewer, Rating::Ok));
    }

    #[test]
    fn software_encoder_with_low_fps_is_driver() {
        let s = Sample {
            source_fps: 11.0,
            encoder: "libx264",
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::Driver, Rating::Weak));
        // A hardware encoder with low fps is an idle screen, not a verdict.
        let s = Sample {
            source_fps: 11.0,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::None, Rating::Good));
    }

    #[test]
    fn missing_stats_do_not_panic() {
        let s = Sample {
            queue_ms: None,
            requested_fps: 0,
            source_fps: 0.0,
            ..good()
        };
        assert_eq!(stage_rating(&s), (Stage::None, Rating::Good));
    }

    #[test]
    fn hysteresis_needs_equal_windows() {
        let mut b = Bottleneck::default();
        let bad = Sample {
            loss_pct: 10.0,
            ..good()
        };
        b.update(&bad);
        b.update(&bad);
        assert_eq!(b.verdict().stage, Stage::None);
        b.update(&bad);
        assert_eq!(
            b.verdict(),
            Verdict {
                stage: Stage::Network,
                rating: Rating::Weak
            }
        );
        assert!(b.line().starts_with("Bottleneck: network - weak"));
    }

    #[test]
    fn a_flicker_does_not_change_the_verdict() {
        let mut b = Bottleneck::default();
        let bad = Sample {
            loss_pct: 10.0,
            ..good()
        };
        for _ in 0..10 {
            b.update(&bad);
            b.update(&good());
            b.update(&good());
        }
        assert_eq!(b.verdict().stage, Stage::None);
    }
}
