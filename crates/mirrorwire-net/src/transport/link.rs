//! Presenter link control: the FEC setting, the viewer's link reports and the
//! adaptive bitrate budget derived from them.

use mirrorwire_proto::control::QueueDelay;
use mirrorwire_proto::fec::FecMode;
use mirrorwire_proto::rate::{AdaptiveBitrate, LinkMemory, Sample};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ceiling for [`LinkState::boost_pct`].
const MAX_FEC_BOOST: f32 = 15.0;
/// How long the presenter remembers a viewer's link after it left. Only in
/// memory: a link's capacity belongs to this path and this hour, and a stale
/// value would overrun the link at connect, which slow start exists to avoid.
const REMEMBER_LINK: Duration = Duration::from_secs(10 * 60);
/// Viewers whose link is remembered at once (oldest forgotten first).
const REMEMBERED_PEERS: usize = 16;

/// One viewer on one path: its certificate fingerprint (as pairing uses)
/// and its address, since the same tablet over USB and over Wi-Fi are two
/// different links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    fingerprint: String,
    ip: IpAddr,
}

impl Peer {
    pub fn new(fingerprint: &str, ip: IpAddr) -> Self {
        Self {
            fingerprint: fingerprint.to_ascii_lowercase(),
            ip: ip.to_canonical(),
        }
    }
}

/// The current viewer and what the controller learned about earlier ones.
#[derive(Debug, Default)]
struct Peers {
    current: Option<Peer>,
    links: Vec<(Peer, LinkMemory, Instant)>,
}

impl Peers {
    fn remember(&mut self, peer: Peer, memory: LinkMemory, now: Instant) {
        self.links.retain(|(p, ..)| *p != peer);
        if self.links.len() >= REMEMBERED_PEERS {
            self.links.remove(0);
        }
        self.links.push((peer, memory, now));
    }

    fn recall(&mut self, peer: &Peer, now: Instant) -> Option<LinkMemory> {
        self.links
            .retain(|(_, _, at)| now.saturating_duration_since(*at) < REMEMBER_LINK);
        let i = self.links.iter().position(|(p, ..)| p == peer)?;
        Some(self.links.remove(i).1)
    }
}

/// Presenter-side link control, shared with the app: the FEC setting, what
/// the current viewer last reported about the link, and the adaptive
/// bitrate budget (video + parity) derived from it.
#[derive(Debug)]
pub struct LinkControl {
    /// Locked before `state` when both are needed.
    peers: Mutex<Peers>,
    state: Mutex<LinkState>,
    started: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct LinkState {
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
    /// This viewer was here before on this path and its link failed at
    /// this budget (kbit/s); the controller resumed from it.
    pub resumed_bad_kbps: Option<u32>,
}

impl Default for LinkState {
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
            resumed_bad_kbps: None,
        }
    }
}

impl LinkState {
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
        match (self.rate.last_cut(), self.resumed_bad_kbps) {
            (Some(cut), _) if self.adaptive => {
                line += &format!("  ·  last cut: {}", cut.reason());
            }
            (None, Some(bad)) if self.adaptive => {
                line += &format!(
                    "  ·  resumed: this link failed at {:.1} Mbit/s last time",
                    bad as f32 / 1000.0
                );
            }
            _ => {}
        }
        line
    }

    /// Encoder target: the budget minus room for delta-frame parity.
    pub fn encoder_kbps(&self) -> u32 {
        mirrorwire_proto::rate::encoder_kbps(self.budget_kbps(), self.overheads().map(|o| o.0))
    }
}

impl LinkControl {
    pub fn new(mode: FecMode) -> Arc<Self> {
        Arc::new(Self {
            peers: Mutex::new(Peers::default()),
            state: Mutex::new(LinkState {
                mode,
                ..Default::default()
            }),
            started: Instant::now(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn snapshot(&self) -> LinkState {
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

    /// The encoder target for the next frames (kbit/s), see [`LinkState::encoder_kbps`].
    pub fn encoder_kbps(&self) -> u32 {
        self.lock().encoder_kbps()
    }

    /// Total budget on the wire (kbit/s), see [`LinkState::budget_kbps`].
    pub fn budget_kbps(&self) -> u32 {
        self.lock().budget_kbps()
    }

    fn lock_peers(&self) -> std::sync::MutexGuard<'_, Peers> {
        self.peers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A viewer connected (it replaces any other): remember the previous
    /// viewer's link, and start from this one's if it was here before on
    /// the same path ([`AdaptiveBitrate::resume`]).
    pub(super) fn viewer_connected(&self, peer: Peer) {
        let now = Instant::now();
        let mut peers = self.lock_peers();
        let mut s = self.lock();
        if let Some(prev) = peers.current.take() {
            peers.remember(prev, s.rate.memory(), now);
        }
        let ceiling = s.rate.ceiling_kbps();
        let memory = peers.recall(&peer, now);
        let rate = memory.map_or_else(
            || AdaptiveBitrate::new(ceiling),
            |m| AdaptiveBitrate::resume(ceiling, m),
        );
        let resumed_bad_kbps = memory.and_then(|m| m.bad_kbps);
        if let Some(bad) = resumed_bad_kbps {
            tracing::info!(
                "adaptive bitrate: viewer seen before on this path, start {:.1} Mbit/s (failed at {:.1})",
                rate.budget_kbps() as f32 / 1000.0,
                bad as f32 / 1000.0
            );
        }
        peers.current = Some(peer);
        *s = LinkState {
            mode: s.mode,
            adaptive: s.adaptive,
            rate,
            resumed_bad_kbps,
            ..Default::default()
        };
    }

    /// A viewer's session ended: remember its link (unless another viewer
    /// already replaced it, which saved it then).
    pub(super) fn viewer_left(&self, peer: &Peer) {
        let mut peers = self.lock_peers();
        if peers.current.as_ref() == Some(peer) {
            let memory = self.lock().rate.memory();
            peers.current = None;
            peers.remember(peer.clone(), memory, Instant::now());
        }
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
fn log_cut(s: &LinkState) {
    if let Some(cut) = s.rate.cut_by_last_sample().filter(|_| s.adaptive) {
        tracing::info!("adaptive bitrate: {cut}");
    }
}

/// `bitrate 18.0 of 40.0 Mbit/s (adaptive), encoder 15.2, rtt 6 ms`
pub fn rate_summary(s: &LinkState) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn peer(fp: &str, last: u8) -> Peer {
        Peer::new(fp, IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)))
    }

    /// Cut the budget once by reporting heavy loss with a standing queue.
    fn overrun(link: &LinkControl) {
        let q = QueueDelay {
            mean_us: 80_000,
            max_us: 120_000,
            trend_us_per_s: 0,
        };
        link.on_report(300, 0, 0, Some(q));
    }

    #[test]
    fn a_returning_viewer_resumes_on_the_same_path_only() {
        let link = LinkControl::new(FecMode::Auto);
        link.set_ceiling(40_000);
        let tablet = peer("AB:CD", 2);
        link.viewer_connected(tablet.clone());
        assert_eq!(link.budget_kbps(), 10_000);
        overrun(&link);
        let after_cut = link.budget_kbps();
        assert!(after_cut < 10_000, "{after_cut}");
        link.viewer_left(&tablet);

        // Same fingerprint (any case), same address: resumed below the failure.
        link.viewer_connected(peer("ab:cd", 2));
        let s = link.snapshot();
        assert_eq!(s.resumed_bad_kbps, Some(10_000));
        assert_eq!(s.budget_kbps(), after_cut.min(6_000));
        assert!(s.bitrate_line().contains("resumed"), "{}", s.bitrate_line());

        // The memory is used once; the same viewer over another path
        // (USB instead of Wi-Fi) starts fresh.
        link.viewer_connected(peer("ab:cd", 3));
        let s = link.snapshot();
        assert_eq!((s.budget_kbps(), s.resumed_bad_kbps), (10_000, None));
    }

    #[test]
    fn a_replaced_viewer_is_remembered_and_its_late_exit_is_ignored() {
        let link = LinkControl::new(FecMode::Auto);
        let (a, b) = (peer("aa", 2), peer("bb", 3));
        link.viewer_connected(a.clone());
        overrun(&link);
        // B replaces A; A's session ends afterwards and must not store B's state.
        link.viewer_connected(b.clone());
        link.viewer_left(&a);
        assert_eq!(link.snapshot().resumed_bad_kbps, None);
        link.viewer_connected(a);
        assert_eq!(link.snapshot().resumed_bad_kbps, Some(10_000));
    }

    #[test]
    fn peers_forget_old_and_surplus_links() {
        let now = Instant::now();
        let m = LinkMemory {
            good_kbps: 5_000,
            bad_kbps: Some(6_000),
        };
        let mut peers = Peers::default();
        for i in 0..=REMEMBERED_PEERS as u8 {
            peers.remember(peer("x", i), m, now);
        }
        assert_eq!(peers.links.len(), REMEMBERED_PEERS);
        assert_eq!(peers.recall(&peer("x", 0), now), None);
        assert_eq!(peers.recall(&peer("x", 1), now), Some(m));
        let later = now + REMEMBER_LINK;
        assert_eq!(peers.recall(&peer("x", 2), later), None);
    }
}
