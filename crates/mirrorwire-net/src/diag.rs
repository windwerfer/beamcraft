//! Connection diagnostics shared by the presenter and the viewer GUI.
//!
//! Stages, timestamps and short advice strings are meant to be shown in the
//! window so a user can photograph them; they are also written to the log.

use crate::pairing::ConnectInfo;
use mirrorwire_proto::DEFAULT_PORT;

pub use crate::firewall::FirewallHint;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the viewer waits for a QUIC handshake with one address.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(7);

const MAX_ATTEMPTS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Active,
    Ok,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectStepId {
    Resolve,
    Handshake,
    Cert,
    Pairing,
    Control,
    Decoder,
    FirstDatagram,
    FirstFrame,
}

impl ConnectStepId {
    pub fn all() -> &'static [Self] {
        &[
            Self::Resolve,
            Self::Handshake,
            Self::Cert,
            Self::Pairing,
            Self::Control,
            Self::Decoder,
            Self::FirstDatagram,
            Self::FirstFrame,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Resolve => "Target address",
            Self::Handshake => "QUIC handshake",
            Self::Cert => "Certificate fingerprint",
            Self::Pairing => "Pairing",
            Self::Control => "Control stream",
            Self::Decoder => "Decoder",
            Self::FirstDatagram => "First datagram",
            Self::FirstFrame => "First frame decoded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectStep {
    pub id: ConnectStepId,
    pub state: StepState,
    pub at: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ConnectProgress {
    pub steps: Vec<ConnectStep>,
    pub error: Option<String>,
    pub advice: Option<String>,
}

impl ConnectProgress {
    pub fn new() -> Self {
        Self {
            steps: ConnectStepId::all()
                .iter()
                .map(|id| ConnectStep {
                    id: *id,
                    state: StepState::Pending,
                    at: None,
                    detail: String::new(),
                })
                .collect(),
            error: None,
            advice: None,
        }
    }
}

impl Default for ConnectProgress {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe progress the GUI can poll while a connect attempt runs.
#[derive(Clone)]
pub struct ConnectTrace {
    inner: Arc<Mutex<ConnectProgress>>,
}

impl Default for ConnectTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectTrace {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ConnectProgress::new())),
        }
    }

    pub fn snapshot(&self) -> ConnectProgress {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn start(&self, id: ConnectStepId, detail: impl Into<String>) {
        let detail = detail.into();
        tracing::info!("connect: {} started: {detail}", id.label());
        self.set(id, StepState::Active, detail);
    }

    pub fn ok(&self, id: ConnectStepId, detail: impl Into<String>) {
        let detail = detail.into();
        tracing::info!("connect: {} ok: {detail}", id.label());
        self.set(id, StepState::Ok, detail);
    }

    pub fn fail(&self, id: ConnectStepId, detail: impl Into<String>) {
        let detail = detail.into();
        tracing::warn!("connect: {} failed: {detail}", id.label());
        self.set(id, StepState::Failed, detail);
    }

    pub fn set_error(&self, error: impl Into<String>, advice: Option<String>) {
        let error = error.into();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.error = Some(error);
        g.advice = advice;
    }

    fn set(&self, id: ConnectStepId, state: StepState, detail: String) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(step) = g.steps.iter_mut().find(|s| s.id == id) {
            step.state = state;
            step.at = Some(local_stamp());
            step.detail = detail;
        }
    }
}

/// An incoming viewer seen by the presenter, successful or not.
#[derive(Debug, Clone)]
pub struct IncomingAttempt {
    pub at: String,
    pub peer: String,
    pub viewer_name: Option<String>,
    pub ok: bool,
    pub outcome: String,
}

impl IncomingAttempt {
    pub fn new(
        peer: impl fmt::Display,
        viewer_name: Option<String>,
        ok: bool,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            at: local_stamp(),
            peer: peer.to_string(),
            viewer_name,
            ok,
            outcome: outcome.into(),
        }
    }
}

pub fn push_attempt(log: &Arc<Mutex<Vec<IncomingAttempt>>>, attempt: IncomingAttempt) {
    tracing::info!(
        "presenter attempt from {}{}: {}",
        attempt.peer,
        attempt
            .viewer_name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default(),
        attempt.outcome
    );
    let mut g = log.lock().unwrap_or_else(|e| e.into_inner());
    g.push(attempt);
    while g.len() > MAX_ATTEMPTS {
        g.remove(0);
    }
}

/// Human-readable advice when QUIC never gets a reply, even though mDNS worked.
pub fn no_answer_message(addrs: &[SocketAddr], cause: Option<&str>) -> String {
    let port = addrs.first().map(|a| a.port()).unwrap_or(DEFAULT_PORT);
    let targets = if addrs.is_empty() {
        format!("the presenter (UDP {port})")
    } else {
        addrs
            .iter()
            .map(|a| format!("{a} (UDP)"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let cause = match cause.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) if !c.ends_with('.') => format!("{c}. "),
        Some(c) => format!("{c} "),
        None => String::new(),
    };
    format!(
        "No answer from {targets}. {cause}The presenter is visible via mDNS but QUIC packets aren't getting through. Probably a firewall on the presenter.\n{}",
        crate::firewall::guide_text(port)
    )
}

/// True when the error is a pairing/fingerprint/protocol rejection, not a network failure.
pub fn is_logical_reject(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}").to_ascii_lowercase();
    s.contains("fingerprint")
        || s.contains("rejected")
        || s.contains("pairing")
        || s.contains("protocol")
        || s.contains("unsupported")
}

/// True when nothing useful came back from the far end (timeout, reset, …).
pub fn is_unreachable(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}").to_ascii_lowercase();
    s.contains("timed out")
        || s.contains("timeout")
        || s.contains("reset by peer")
        || s.contains("connection reset")
        || s.contains("no answer from")
}

/// Format every address a viewer will try, in order.
pub fn format_candidates(target: &ConnectInfo) -> String {
    target
        .candidates()
        .into_iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Local wall-clock stamp for the GUI and logs (`HH:MM:SS.mmm`).
pub fn local_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let ms = now.subsec_millis();
    match local_hms(secs) {
        Some((h, m, s)) => format!("{h:02}:{m:02}:{s:02}.{ms:03}"),
        None => format!("{secs}.{ms:03}"),
    }
}

/// Local `(hour, minute, second)` for a Unix time, from the system time zone (`TZ`, else
/// `/etc/localtime`). The zone is read once per process; `None` when it can't be read (no
/// zone file, e.g. Android or Windows), and the caller prints seconds since the epoch.
fn local_hms(secs: i64) -> Option<(i64, i64, i64)> {
    static ZONE: std::sync::OnceLock<Option<tz::TimeZone>> = std::sync::OnceLock::new();
    let zone = ZONE.get_or_init(system_zone).as_ref()?;
    let offset = zone.find_local_time_type(secs).ok()?.ut_offset();
    Some(hms_of_day(secs.checked_add(i64::from(offset))?))
}

/// The zone libc's `localtime` would use: `TZ` if set (empty means UTC), else
/// `/etc/localtime`.
fn system_zone() -> Option<tz::TimeZone> {
    match std::env::var("TZ") {
        Ok(s) if s.is_empty() => Some(tz::TimeZone::utc()),
        Ok(s) => tz::TimeZone::from_posix_tz(&s).ok(),
        Err(_) => tz::TimeZone::local().ok(),
    }
}

/// Hours, minutes and seconds into the day of a (zone-shifted) Unix time.
fn hms_of_day(local_secs: i64) -> (i64, i64, i64) {
    let day = local_secs.rem_euclid(86_400);
    (day / 3600, day % 3600 / 60, day % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn hms_of_day_wraps_and_handles_times_before_1970() {
        assert_eq!(hms_of_day(0), (0, 0, 0));
        assert_eq!(hms_of_day(86_399), (23, 59, 59));
        assert_eq!(hms_of_day(86_400 + 3_723), (1, 2, 3));
        assert_eq!(hms_of_day(-1), (23, 59, 59));
    }

    #[test]
    fn local_stamp_has_a_clock_or_epoch_shape() {
        let s = local_stamp();
        let (head, ms) = s.rsplit_once('.').expect("millis");
        assert_eq!(ms.len(), 3, "{s}");
        assert!(head.len() == 8 && head.matches(':').count() == 2 || head.parse::<i64>().is_ok());
    }

    #[test]
    fn timeout_message_mentions_udp_and_firewall_cmd() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 201, 132)), 47_800);
        let msg = no_answer_message(&[addr], None);
        assert!(msg.contains("192.168.201.132:47800 (UDP)"), "{msg}");
        assert!(
            msg.contains("sudo firewall-cmd --add-port=47800/udp"),
            "{msg}"
        );
        assert!(
            msg.contains("Ubuntu with ufw: sudo ufw allow 47800/udp"),
            "{msg}"
        );
        assert!(msg.contains("mDNS"), "{msg}");
        assert!(msg.contains("QUIC"), "{msg}");
    }

    #[test]
    fn timeout_message_lists_every_address() {
        let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)), 47_800);
        let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 47_800);
        let msg = no_answer_message(&[a, b], Some("timed out after 7s"));
        assert!(msg.contains("192.168.1.2:47800 (UDP)"), "{msg}");
        assert!(msg.contains("10.0.0.5:47800 (UDP)"), "{msg}");
        assert!(msg.contains("timed out after 7s"), "{msg}");
    }

    #[test]
    fn logical_reject_is_not_a_firewall_error() {
        let err =
            anyhow::anyhow!("presenter rejected connection: not paired: pairing code required");
        assert!(is_logical_reject(&err));
        assert!(!is_unreachable(&err));
        let err = anyhow::anyhow!("connecting to 192.168.1.2:47800: timed out");
        assert!(!is_logical_reject(&err));
        assert!(is_unreachable(&err));
    }

    #[test]
    fn local_stamp_has_expected_shape() {
        let s = local_stamp();
        let parts: Vec<_> = s.split([':', '.']).collect();
        assert_eq!(parts.len(), 4, "{s}");
        assert!(
            parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
            "{s}"
        );
    }
}
