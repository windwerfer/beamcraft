//! Presenter side: accept viewers, check pairing, send video and cursor.

use super::link::{rate_summary, LinkControl, LinkState};
use super::{ms, read_body, read_msg, transport_config, version_mismatch, write_msg};
use crate::diag::{push_attempt, IncomingAttempt};
use crate::identity::Identity;
use crate::pairing::PairedViewers;
use crate::tls;
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use mirrorwire_proto::clock::now_us;
use mirrorwire_proto::control::{hello_protocol, Control, StreamInfo};
use mirrorwire_proto::cursor::{CursorEvent, CursorImage, PointerUpdate};
use mirrorwire_proto::fec::FecMode;
use mirrorwire_proto::fragment::{fragment, FrameTiming, PacketHeader};
use mirrorwire_proto::stats::StageStats;
use mirrorwire_proto::PROTOCOL_VERSION;
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Connection, Endpoint};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// Whether a delta frame of `frame_len` bytes should be dropped because the
/// datagram queue is too full to take it (a keyframe is always queued).
pub fn should_drop_for_backlog(keyframe: bool, frame_len: usize, free_space: usize) -> bool {
    !keyframe && free_space < frame_len + frame_len / 16 + 4096
}

/// Limits how often a viewer's keyframe requests force an IDR. While a
/// viewer waits for a keyframe it asks every ~100 ms; on Wi-Fi with a
/// 250 ms stall, honouring each one sent 4–8 IDRs a second into the
/// congested link (33 % loss, 40–50 frames lost per stall on the MTK
/// tablet, 2026-11-15). After a forced keyframe, requests are ignored until
/// it could have arrived: 2× the RTT, at least 500 ms, at most 2 s. The
/// viewer keeps asking, so a lost keyframe is still replaced.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyframeGate {
    last_us: Option<u64>,
    pub suppressed: u64,
}

impl KeyframeGate {
    pub const MIN_GAP_US: u64 = 500_000;
    pub const MAX_GAP_US: u64 = 2_000_000;

    /// Whether a request at `now_us` should force a keyframe.
    pub fn allow(&mut self, now_us: u64, rtt: Duration) -> bool {
        let gap = (rtt.as_micros().min(u64::MAX as u128) as u64)
            .saturating_mul(2)
            .clamp(Self::MIN_GAP_US, Self::MAX_GAP_US);
        if self.last_us.is_some_and(|t| now_us.saturating_sub(t) < gap) {
            self.suppressed += 1;
            return false;
        }
        self.last_us = Some(now_us);
        true
    }
}

#[derive(Debug, Clone)]
pub struct OutgoingFrame {
    /// Consecutive per encoded frame; a gap tells the viewer a frame was lost.
    pub frame_id: u32,
    pub keyframe: bool,
    pub data: Bytes,
    pub capture_us: u64,
    pub encoded_us: u64,
}

pub struct PresenterConfig {
    pub bind: SocketAddr,
    pub stream_info: StreamInfo,
    /// Code an unpaired viewer must present. `None` accepts any viewer.
    pub pairing_code: Option<String>,
    pub config_dir: PathBuf,
    /// Incoming handshake / pairing attempts for the presenter GUI.
    pub attempts: std::sync::Arc<Mutex<Vec<IncomingAttempt>>>,
    /// Cursor position and image changes from the capture source (the
    /// sending half lives with the capture thread).
    pub cursor_events: Option<mpsc::Receiver<CursorEvent>>,
    /// FEC setting and link feedback (see [`LinkControl`]).
    pub fec: Arc<LinkControl>,
}

type Callback = Arc<dyn Fn() + Send + Sync>;

/// The viewer currently receiving the stream.
#[derive(Clone)]
struct ActiveViewer {
    conn: Connection,
}

pub struct Presenter {
    pub frames: mpsc::Sender<OutgoingFrame>,
    pub local_addr: SocketAddr,
    pub viewer_connected: watch::Receiver<bool>,
    stream_info: Arc<watch::Sender<StreamInfo>>,
    pub(super) fec: Arc<LinkControl>,
    endpoint: Endpoint,
}

impl Presenter {
    pub fn start(
        identity: &Identity,
        cfg: PresenterConfig,
        on_keyframe_request: Callback,
    ) -> Result<Self> {
        let tls = tls::server_config(identity)?;
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        server.transport_config(transport_config());
        let endpoint = Endpoint::server(server, cfg.bind).context("binding QUIC endpoint")?;
        let local_addr = endpoint.local_addr()?;

        let (frames, frames_rx) = mpsc::channel::<OutgoingFrame>(8);
        let (current_tx, current_rx) = watch::channel::<Option<ActiveViewer>>(None);
        let (connected_tx, viewer_connected) = watch::channel(false);
        let current_tx = Arc::new(current_tx);
        let mut info = cfg.stream_info;
        info.fec = cfg.fec.snapshot().mode.as_string();
        let stream_info = Arc::new(watch::channel(info).0);

        tokio::spawn(send_loop(
            frames_rx,
            current_rx.clone(),
            on_keyframe_request.clone(),
            cfg.fec.clone(),
        ));
        if let Some(cursor_rx) = cfg.cursor_events {
            tokio::spawn(cursor_loop(cursor_rx, current_rx));
        }

        let shared = Arc::new(SessionShared {
            stream_info: stream_info.clone(),
            pairing_code: cfg.pairing_code,
            paired: Mutex::new(PairedViewers::load(&cfg.config_dir)),
            current: current_tx,
            connected: connected_tx,
            on_keyframe_request,
            attempts: cfg.attempts,
            fec: cfg.fec.clone(),
        });
        let ep = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_viewer(incoming, shared).await {
                        tracing::warn!("viewer session ended: {e:#}");
                    }
                });
            }
        });

        Ok(Self {
            frames,
            local_addr,
            viewer_connected,
            stream_info,
            fec: cfg.fec,
            endpoint,
        })
    }

    /// Change the stream description; connected viewers get a fresh
    /// `stream_info`, new viewers get the new one.
    pub fn update_stream_info(&self, f: impl FnOnce(&mut StreamInfo)) {
        self.stream_info.send_if_modified(|info| {
            let before = info.clone();
            f(info);
            *info != before
        });
    }

    /// Change the FEC setting live; viewers see it in `stream_info.fec`.
    pub fn set_fec_mode(&self, mode: FecMode) {
        self.fec.set_mode(mode);
        self.update_stream_info(|i| i.fec = mode.as_string());
    }

    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"presenter stopped");
    }
}

struct SessionShared {
    stream_info: Arc<watch::Sender<StreamInfo>>,
    pairing_code: Option<String>,
    paired: Mutex<PairedViewers>,
    current: Arc<watch::Sender<Option<ActiveViewer>>>,
    connected: watch::Sender<bool>,
    on_keyframe_request: Callback,
    attempts: Arc<Mutex<Vec<IncomingAttempt>>>,
    fec: Arc<LinkControl>,
}

async fn handle_viewer(incoming: quinn::Incoming, shared: Arc<SessionShared>) -> Result<()> {
    let remote = incoming.remote_address();
    let conn = match incoming.await {
        Ok(c) => c,
        Err(e) => {
            push_attempt(
                &shared.attempts,
                IncomingAttempt::new(remote, None, false, format!("handshake failed: {e:#}")),
            );
            return Err(e.into());
        }
    };
    let remote = conn.remote_address();
    let fp = match tls::peer_fingerprint(&conn) {
        Some(fp) => fp,
        None => {
            push_attempt(
                &shared.attempts,
                IncomingAttempt::new(remote, None, false, "handshake failed: no certificate"),
            );
            bail!("viewer sent no certificate");
        }
    };
    let (mut send, mut recv) = conn.accept_bi().await?;

    // The version is checked before the rest of the hello is parsed, so a
    // viewer of another version gets a readable answer even if the hello
    // itself changed shape.
    let body = read_body(&mut recv).await?;
    match hello_protocol(&body) {
        Some(PROTOCOL_VERSION) => {}
        Some(protocol) => {
            let reason = version_mismatch(protocol);
            write_msg(
                &mut send,
                &Control::Rejected {
                    reason: reason.clone(),
                },
            )
            .await?;
            send.finish()?;
            push_attempt(
                &shared.attempts,
                IncomingAttempt::new(remote, None, false, reason),
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
            bail!("viewer {remote} speaks protocol {protocol}");
        }
        None => {
            push_attempt(
                &shared.attempts,
                IncomingAttempt::new(remote, None, false, "expected hello on control stream"),
            );
            bail!("expected hello from {remote}");
        }
    }
    let Control::Hello {
        name, pairing_code, ..
    } = Control::decode_body(&body)?
    else {
        bail!("expected hello from {remote}");
    };
    let authorized = match &shared.pairing_code {
        None => true,
        Some(code) => {
            let mut paired = shared.paired.lock().expect("paired lock");
            if paired.contains(&fp) {
                true
            } else if pairing_code.as_deref() == Some(code.as_str()) {
                paired.add(&fp, &name)?;
                tracing::info!(
                    "paired new viewer {name} ({})",
                    crate::identity::short_fingerprint(&fp)
                );
                true
            } else {
                false
            }
        }
    };
    if !authorized {
        let reason = "not paired: pairing code required".to_owned();
        write_msg(
            &mut send,
            &Control::Rejected {
                reason: reason.clone(),
            },
        )
        .await?;
        send.finish()?;
        push_attempt(
            &shared.attempts,
            IncomingAttempt::new(
                remote,
                Some(name.clone()),
                false,
                "pairing rejected: wrong or missing code",
            ),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        bail!("rejected unpaired viewer {name} at {remote}");
    }

    let mut info_rx = shared.stream_info.subscribe();
    let stream_info = info_rx.borrow_and_update().clone();
    write_msg(&mut send, &Control::StreamInfo(stream_info)).await?;
    push_attempt(
        &shared.attempts,
        IncomingAttempt::new(remote, Some(name.clone()), true, "connected"),
    );
    tracing::info!("viewer {name} connected from {remote}");
    shared.fec.viewer_connected();
    let active = ActiveViewer { conn: conn.clone() };
    if let Some(old) = shared.current.send_replace(Some(active)) {
        old.conn.close(1u32.into(), b"replaced by another viewer");
    }
    shared.connected.send_replace(true);
    (shared.on_keyframe_request)();

    // One writer owns the send half: pongs from the reader below, and a
    // fresh stream_info whenever the presenter changes it.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Control>();
    let writer = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                m = out_rx.recv() => match m {
                    Some(m) => m,
                    None => return,
                },
                changed = info_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    Control::StreamInfo(info_rx.borrow_and_update().clone())
                }
            };
            if write_msg(&mut send, &msg).await.is_err() {
                return;
            }
        }
    });

    // Not armed by the keyframe forced at connect: if the viewer missed that
    // one, its first request is answered at once (a pre-armed gate cost
    // ~0.5 s of black at startup in the loopback).
    let mut keyframe_gate = KeyframeGate::default();
    let result = async {
        loop {
            let msg = match read_msg(&mut recv).await {
                Ok(m) => m,
                Err(_) if conn.close_reason().is_some() => return Ok(()),
                Err(e) => return Err(e),
            };
            match msg {
                Control::Ping { t_viewer_us } => {
                    let _ = out_tx.send(Control::Pong { t_viewer_us, t_presenter_us: now_us() });
                }
                Control::RequestKeyframe { reason } => {
                    if keyframe_gate.allow(now_us(), conn.rtt()) {
                        tracing::debug!("keyframe requested: {reason}");
                        (shared.on_keyframe_request)();
                    } else {
                        tracing::debug!(
                            "keyframe request ignored ({reason}): one is on its way ({} ignored so far)",
                            keyframe_gate.suppressed
                        );
                    }
                }
                Control::LinkReport { loss_permille, packets, recovered, unrecoverable, queue } => {
                    tracing::debug!(
                        "viewer link: loss {:.1}% of {packets} packets, {recovered} rebuilt, {unrecoverable} frames lost{}",
                        loss_permille as f32 / 10.0,
                        queue.map_or(String::new(), |q| format!(", {q}"))
                    );
                    shared.fec.on_report(loss_permille, recovered, unrecoverable, queue);
                }
                Control::Bye => return Ok::<_, anyhow::Error>(()),
                other => tracing::debug!("ignoring {other:?}"),
            }
        }
    }
    .await;
    writer.abort();

    shared.current.send_if_modified(|cur| {
        if cur
            .as_ref()
            .is_some_and(|c| c.conn.stable_id() == conn.stable_id())
        {
            *cur = None;
            true
        } else {
            false
        }
    });
    if shared.current.borrow().is_none() {
        shared.connected.send_replace(false);
    }
    tracing::info!("viewer {name} disconnected");
    result
}

/// Forwards cursor changes to the current viewer: positions as datagrams
/// (with a fresh sequence number), images on their own unidirectional
/// stream. A newly connected viewer first gets every cached image and the
/// last position, so it can draw the cursor before it moves.
async fn cursor_loop(
    mut events: mpsc::Receiver<CursorEvent>,
    mut current: watch::Receiver<Option<ActiveViewer>>,
) {
    const CACHE: usize = 32;
    let mut seq = 0u32;
    let mut last: Option<PointerUpdate> = None;
    let mut images: std::collections::VecDeque<Arc<CursorImage>> = Default::default();
    let target = |current: &watch::Receiver<Option<ActiveViewer>>| {
        current.borrow().as_ref().map(|v| v.conn.clone())
    };
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                None => return,
                Some(CursorEvent::Image(img)) => {
                    let img = Arc::new(img);
                    images.retain(|i| i.id != img.id);
                    if images.len() >= CACHE {
                        images.pop_front();
                    }
                    images.push_back(img.clone());
                    if let Some(conn) = target(&current) {
                        send_cursor_image(conn, img);
                    }
                }
                Some(CursorEvent::Pointer(mut p)) => {
                    seq = seq.wrapping_add(1);
                    p.seq = seq;
                    last = Some(p);
                    if let Some(conn) = target(&current) {
                        let _ = conn.send_datagram(Bytes::copy_from_slice(&p.encode()));
                    }
                }
            },
            changed = current.changed() => {
                if changed.is_err() {
                    return;
                }
                let Some(conn) = target(&current) else { continue };
                for img in &images {
                    send_cursor_image(conn.clone(), img.clone());
                }
                if let Some(mut p) = last {
                    seq = seq.wrapping_add(1);
                    p.seq = seq;
                    last = Some(p);
                    let _ = conn.send_datagram(Bytes::copy_from_slice(&p.encode()));
                }
            }
        }
    }
}

fn send_cursor_image(conn: Connection, img: Arc<CursorImage>) {
    tokio::spawn(async move {
        let result = async {
            let mut s = conn.open_uni().await?;
            s.write_all(&img.encode()).await?;
            s.finish()?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = result {
            tracing::debug!("cursor image {} not sent: {e:#}", img.id);
        }
    });
}

async fn send_loop(
    mut frames: mpsc::Receiver<OutgoingFrame>,
    current: watch::Receiver<Option<ActiveViewer>>,
    on_keyframe_request: Callback,
    fec: Arc<LinkControl>,
) {
    let mut stats = StageStats::new(&["capture_to_encoded", "encoded_to_sent", "send"]);
    let mut window_start = Instant::now();
    let (mut window_frames, mut window_bytes, mut window_keyframes) = (0u32, 0usize, 0u32);
    let mut backlog_drops = 0u32;
    let (mut window_parity, mut window_packets) = (0usize, 0usize);
    // Bytes handed to QUIC this window (video + parity + headers).
    let mut window_wire = 0usize;
    // QUIC's own packet counters at the last window: (connection, sent, lost).
    let mut quic_prev = (0usize, 0u64, 0u64);

    while let Some(frame) = frames.recv().await {
        let Some(conn) = current.borrow().as_ref().map(|v| v.conn.clone()) else {
            continue;
        };
        let Some(max) = conn.max_datagram_size() else {
            continue;
        };
        let sent_us = now_us();
        let timing = FrameTiming {
            capture_us: frame.capture_us,
            encoded_us: frame.encoded_us,
            sent_us,
        };
        let overhead = fec.overhead(frame.keyframe);
        let packets = fragment(
            frame.frame_id,
            frame.keyframe,
            timing,
            &frame.data,
            max,
            overhead,
        );
        let wire_len: usize = packets.iter().map(|p| p.len()).sum();
        if should_drop_for_backlog(frame.keyframe, wire_len, conn.datagram_send_buffer_space()) {
            // The network is behind (big scene change on Wi-Fi). Sending this
            // frame would only add latency; the viewer recovers with the
            // keyframe we ask for instead.
            backlog_drops += 1;
            tracing::debug!(
                "network backlog: dropped frame {} ({} bytes)",
                frame.frame_id,
                frame.data.len()
            );
            on_keyframe_request();
            continue;
        }
        // Burst every fragment (parity last); the congestion controller's
        // pacer spreads them out. send_datagram queues immediately; only wait
        // when the datagram send buffer is full so a frame is not paced out
        // one packet at a time.
        let mut send_err = false;
        window_packets += packets.len();
        window_wire += wire_len;
        if overhead.is_some() {
            window_parity += packets
                .first()
                .and_then(|p| PacketHeader::parse(p))
                .map_or(0, |(h, _)| h.parity_count as usize);
        }
        for datagram in packets {
            if conn.send_datagram(datagram.clone()).is_ok() {
                continue;
            }
            if let Err(e) = conn.send_datagram_wait(datagram).await {
                tracing::debug!("datagram send failed: {e}");
                send_err = true;
                break;
            }
        }
        let done_us = now_us();
        let send_ms = ms(done_us, sent_us);
        if send_ms > 8.0 || send_err {
            tracing::info!(
                "QUIC send of frame {} took {send_ms:.1} ms{}",
                frame.frame_id,
                if send_err { " (truncated)" } else { "" }
            );
        }
        stats.record("capture_to_encoded", ms(frame.encoded_us, frame.capture_us));
        stats.record("encoded_to_sent", ms(sent_us, frame.encoded_us));
        stats.record("send", ms(done_us, sent_us));
        window_frames += 1;
        window_bytes += frame.data.len();
        window_keyframes += u32::from(frame.keyframe);

        let elapsed = window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let secs = elapsed.as_secs_f64();
            // Loss as QUIC sees it (packets never acknowledged): on-air loss,
            // as opposed to what the viewer counts after reassembly.
            let path = conn.stats().path;
            if quic_prev.0 != conn.stable_id() {
                quic_prev = (conn.stable_id(), 0, 0);
            }
            let (quic_sent, quic_lost) = (
                path.sent_packets.saturating_sub(quic_prev.1),
                path.lost_packets.saturating_sub(quic_prev.2),
            );
            quic_prev = (conn.stable_id(), path.sent_packets, path.lost_packets);
            tracing::info!(
                "presenter: {:.1} fps, {:.2} Mbit/s, {} keyframes, {} dropped for backlog, mtu {}, quic lost {} of {} packets, cwnd {} KiB, {}, ms mean/p95/max:{}",
                window_frames as f64 / secs,
                window_bytes as f64 * 8.0 / secs / 1e6,
                window_keyframes,
                backlog_drops,
                max,
                quic_lost,
                quic_sent,
                path.cwnd / 1024,
                fec_summary(&fec.snapshot(), window_parity, window_packets),
                stats.report()
            );
            fec.on_send_window(
                backlog_drops,
                (window_wire as f64 * 8.0 / secs / 1000.0) as f32,
                conn.rtt(),
            );
            backlog_drops = 0;
            window_wire = 0;
            (window_parity, window_packets) = (0, 0);
            stats.clear();
            window_start = Instant::now();
            (window_frames, window_bytes, window_keyframes) = (0, 0, 0);
        }
    }
}

fn fec_summary(s: &LinkState, parity: usize, packets: usize) -> String {
    format!("{}, {}", rate_summary(s), fec_part(s, parity, packets))
}

fn fec_part(s: &LinkState, parity: usize, packets: usize) -> String {
    match s.overheads() {
        None => "fec off".into(),
        Some(_) => format!(
            "fec {} ~{:.0}% parity packets (boost +{:.0}), viewer loss {:.1}%, rebuilt {:.0}/s, unrecoverable {}",
            s.mode.as_string(),
            100.0 * parity as f64 / packets.saturating_sub(parity).max(1) as f64,
            s.boost_pct,
            s.loss_pct,
            s.recovered_per_s,
            s.unrecoverable
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::DATAGRAM_SEND_BUFFER;

    #[test]
    fn keyframe_requests_are_limited_while_one_is_on_its_way() {
        let mut g = KeyframeGate::default();
        let lan = Duration::from_millis(5);
        assert!(g.allow(0, lan), "first request");
        // The viewer re-asks every 100 ms: ignored for the 500 ms minimum.
        for t in (100_000..500_000).step_by(100_000) {
            assert!(!g.allow(t, lan), "{t}");
        }
        assert!(g.allow(500_000, lan), "a lost keyframe is replaced");
        assert_eq!(g.suppressed, 4);
        // A Wi-Fi stall (RTT 400 ms): wait 2 RTTs.
        let stall = Duration::from_millis(400);
        assert!(!g.allow(1_200_000, stall));
        assert!(g.allow(1_300_000, stall));
        // Capped at 2 s, and a garbage RTT can't block forever.
        let huge = Duration::from_secs(3_600);
        assert!(!g.allow(3_299_999, huge));
        assert!(g.allow(3_300_000, huge));
        // A clock step back counts as "just sent", not a panic.
        assert!(!g.allow(0, lan));
    }

    #[test]
    fn backlog_drops_only_delta_frames() {
        assert!(!should_drop_for_backlog(
            false,
            50_000,
            DATAGRAM_SEND_BUFFER
        ));
        assert!(should_drop_for_backlog(false, 50_000, 40_000));
        assert!(
            !should_drop_for_backlog(true, 500_000, 0),
            "keyframes always go out"
        );
    }
}
