//! Viewer-side pacing of `request_keyframe` while it waits for a keyframe.
//!
//! The first request goes out at once; if no keyframe arrives, the next one follows after
//! [`KeyframeBackoff::FIRST_US`], then the wait doubles up to [`KeyframeBackoff::MAX_US`].
//! The presenter forces at most one keyframe per gap anyway (`KeyframeGate`), so a tight
//! retry loop (the old 100 ms) only adds control traffic on a link that is already losing
//! packets, and a presenter that answered with an IDR is not asked again before that IDR
//! could have arrived.

#[derive(Debug, Default, Clone, Copy)]
pub struct KeyframeBackoff {
    /// When the last request went out and how long to wait before the next one.
    last: Option<(u64, u64)>,
    /// Requests sent since the last keyframe arrived.
    pub requests: u32,
}

impl KeyframeBackoff {
    pub const FIRST_US: u64 = 250_000;
    pub const MAX_US: u64 = 1_000_000;

    /// Whether a request should be sent at `now_us`; records it if so.
    pub fn should_request(&mut self, now_us: u64) -> bool {
        let wait = match self.last {
            None => 0,
            Some((at, wait)) if now_us.saturating_sub(at) < wait => return false,
            Some((_, wait)) => wait,
        };
        let next = if self.last.is_none() {
            Self::FIRST_US
        } else {
            wait.saturating_mul(2).min(Self::MAX_US)
        };
        self.last = Some((now_us, next));
        self.requests += 1;
        true
    }

    /// A keyframe arrived (or recovery is over): the next loss starts again at once.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_from_250_ms_to_1_s() {
        let mut b = KeyframeBackoff::default();
        let mut sent = Vec::new();
        // Poll every 10 ms for 5 s.
        for t in (0..5_000_000u64).step_by(10_000) {
            if b.should_request(t) {
                sent.push(t / 1000);
            }
        }
        assert_eq!(sent, [0, 250, 750, 1750, 2750, 3750, 4750]);
    }

    #[test]
    fn reset_restarts_at_once() {
        let mut b = KeyframeBackoff::default();
        assert!(b.should_request(0));
        assert!(!b.should_request(100_000));
        b.reset();
        assert!(b.should_request(100_000));
        assert_eq!(b.requests, 1);
    }
}
