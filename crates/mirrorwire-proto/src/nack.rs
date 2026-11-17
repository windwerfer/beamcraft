//! Selective retransmission of lost video fragments.
//!
//! The viewer reports the fragments it still needs after FEC has done what it can
//! ([`crate::fragment::Reassembler::take_nacks`]) in one [`Nack`] datagram; the presenter answers
//! from a [`ResendWindow`] of recently sent packets, within a small byte budget. Everything here
//! is pure state (no I/O, no clock), so the sender and the receiver share one tested
//! implementation.

use crate::be;
use crate::fragment::mark_resent;
use bytes::Bytes;
use std::collections::VecDeque;

/// First byte of a NACK datagram (viewer → presenter). Video packets start with
/// [`crate::fragment::WIRE_VERSION`] (2) and pointer datagrams with 0xC1.
pub const NACK_DATAGRAM_KIND: u8 = 0xC2;
pub const NACK_VERSION: u8 = 1;
/// A NACK datagram never exceeds this (it must fit one QUIC datagram).
pub const MAX_NACK_LEN: usize = 1000;
const HEAD_LEN: usize = 3;

/// The fragments of one frame the viewer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NackEntry {
    pub frame_id: u32,
    /// Packet indices (`0..data_count` data shards). Empty = the whole frame (every data
    /// shard), used when no packet of the frame arrived and its layout is unknown.
    pub indices: Vec<u16>,
}

impl NackEntry {
    fn wire_len(&self) -> usize {
        4 + 1 + 2 * self.indices.len()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nack {
    pub entries: Vec<NackEntry>,
}

impl Nack {
    /// Adds an entry unless the datagram would get too big (returns `false`; the entry is
    /// left for the next NACK).
    pub fn try_push(&mut self, e: NackEntry) -> bool {
        let used: usize = HEAD_LEN + self.entries.iter().map(NackEntry::wire_len).sum::<usize>();
        if e.indices.len() > u8::MAX as usize
            || self.entries.len() >= u8::MAX as usize
            || used + e.wire_len() > MAX_NACK_LEN
        {
            return false;
        }
        self.entries.push(e);
        true
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of fragments asked for (a whole-frame entry counts as one).
    pub fn requested(&self) -> usize {
        self.entries.iter().map(|e| e.indices.len().max(1)).sum()
    }

    /// | Offset | Size | Field |
    /// |---|---|---|
    /// | 0 | 1 | kind `0xC2` |
    /// | 1 | 1 | version 1 |
    /// | 2 | 1 | `n` entries (1..=255) |
    /// | 3 | … | per entry: `frame_id` u32, `count` u8 (0 = whole frame), `count` × index u16 |
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![NACK_DATAGRAM_KIND, NACK_VERSION, self.entries.len() as u8];
        for e in &self.entries {
            out.extend_from_slice(&e.frame_id.to_be_bytes());
            out.push(e.indices.len() as u8);
            for i in &e.indices {
                out.extend_from_slice(&i.to_be_bytes());
            }
        }
        out
    }

    /// Parses a NACK datagram; anything malformed (wrong kind or version, truncated, trailing
    /// bytes, no entries) is `None`.
    pub fn decode(d: &[u8]) -> Option<Self> {
        if d.len() > MAX_NACK_LEN || *d.first()? != NACK_DATAGRAM_KIND || *d.get(1)? != NACK_VERSION
        {
            return None;
        }
        let n = *d.get(2)? as usize;
        let mut at = HEAD_LEN;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let frame_id = u32::from_be_bytes(be(d, at)?);
            let count = *d.get(at + 4)? as usize;
            at += 5;
            let mut indices = Vec::with_capacity(count);
            for _ in 0..count {
                indices.push(u16::from_be_bytes(be(d, at)?));
                at += 2;
            }
            entries.push(NackEntry { frame_id, indices });
        }
        (n > 0 && at == d.len()).then_some(Self { entries })
    }
}

pub fn is_nack_datagram(d: &[u8]) -> bool {
    d.first() == Some(&NACK_DATAGRAM_KIND)
}

/// How long the presenter keeps sent packets for resending.
pub const RESEND_WINDOW_US: u64 = 3_000_000;
/// Upper bound on the memory of the window.
pub const RESEND_WINDOW_BYTES: usize = 8 << 20;
/// Sustained resend rate (bytes per second). Resends compete with video on the same link, so
/// they are capped; what does not fit is left to the viewer's keyframe fallback.
pub const RESEND_BUDGET_BPS: u64 = 128 * 1024;
/// Largest burst of resends (the token bucket's size).
pub const RESEND_BURST_BYTES: u64 = 64 * 1024;

struct Stored {
    frame_id: u32,
    sent_us: u64,
    data_count: usize,
    bytes: usize,
    packets: Vec<Bytes>,
}

/// What the resend window did, for the presenter's log line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResendStats {
    /// Packets asked for.
    pub requested: u64,
    pub resent: u64,
    /// Not resent: the byte budget was used up.
    pub over_budget: u64,
    /// Not resent: frame already out of the window, or never sent.
    pub unknown: u64,
}

/// The presenter's recently sent packets (~3 s, at most 8 MiB) and the resend budget.
pub struct ResendWindow {
    frames: VecDeque<Stored>,
    bytes: usize,
    tokens: f64,
    last_us: Option<u64>,
    pub stats: ResendStats,
}

impl Default for ResendWindow {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            tokens: RESEND_BURST_BYTES as f64,
            last_us: None,
            stats: ResendStats::default(),
        }
    }
}

impl ResendWindow {
    /// Remembers the packets of a frame just sent (data first, then parity, as
    /// [`crate::fragment::fragment`] returns them).
    pub fn insert(&mut self, frame_id: u32, data_count: usize, packets: Vec<Bytes>, now_us: u64) {
        let bytes = packets.iter().map(Bytes::len).sum();
        self.bytes += bytes;
        self.frames.push_back(Stored {
            frame_id,
            sent_us: now_us,
            data_count,
            bytes,
            packets,
        });
        while let Some(f) = self.frames.front() {
            if self.bytes > RESEND_WINDOW_BYTES
                || f.sent_us.saturating_add(RESEND_WINDOW_US) < now_us
            {
                self.bytes = self.bytes.saturating_sub(f.bytes);
                self.frames.pop_front();
            } else {
                break;
            }
        }
    }

    /// Forgets everything (a new viewer starts from a keyframe).
    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The packets to send for `nack`, marked as resent, as far as the budget allows.
    pub fn resend(&mut self, nack: &Nack, now_us: u64) -> Vec<Bytes> {
        let dt = now_us.saturating_sub(self.last_us.unwrap_or(now_us));
        self.last_us = Some(now_us);
        self.tokens = (self.tokens + dt as f64 * RESEND_BUDGET_BPS as f64 / 1e6)
            .min(RESEND_BURST_BYTES as f64);
        let mut out = Vec::new();
        for e in &nack.entries {
            let Some(frame) = self.frames.iter().rev().find(|f| f.frame_id == e.frame_id) else {
                self.stats.unknown += e.indices.len().max(1) as u64;
                continue;
            };
            let all: Vec<u16>;
            let wanted: &[u16] = if e.indices.is_empty() {
                all = (0..frame.data_count.min(u16::MAX as usize) as u16).collect();
                &all
            } else {
                &e.indices
            };
            for &i in wanted {
                self.stats.requested += 1;
                let Some(p) = frame.packets.get(i as usize) else {
                    self.stats.unknown += 1;
                    continue;
                };
                if self.tokens < p.len() as f64 {
                    self.stats.over_budget += 1;
                    continue;
                }
                self.tokens -= p.len() as f64;
                self.stats.resent += 1;
                out.push(mark_resent(p));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{fragment, FrameTiming, PacketHeader};

    const T: FrameTiming = FrameTiming {
        capture_us: 0,
        encoded_us: 0,
        sent_us: 0,
    };

    fn nack(frame_id: u32, indices: &[u16]) -> Nack {
        Nack {
            entries: vec![NackEntry {
                frame_id,
                indices: indices.to_vec(),
            }],
        }
    }

    #[test]
    fn datagram_roundtrip_and_limits() {
        let n = Nack {
            entries: vec![
                NackEntry {
                    frame_id: 7,
                    indices: vec![0, 5, 600],
                },
                NackEntry {
                    frame_id: u32::MAX,
                    indices: vec![],
                },
            ],
        };
        let wire = n.encode();
        assert_eq!(wire[0], NACK_DATAGRAM_KIND);
        assert!(is_nack_datagram(&wire));
        assert_eq!(Nack::decode(&wire), Some(n.clone()));
        assert_eq!(n.requested(), 4);
        // Every prefix and any trailing byte is rejected.
        for cut in 0..wire.len() {
            assert_eq!(Nack::decode(&wire[..cut]), None, "{cut}");
        }
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(Nack::decode(&long), None);
        assert_eq!(Nack::decode(&[NACK_DATAGRAM_KIND, 2, 1]), None, "version");
        assert_eq!(
            Nack::decode(&[NACK_DATAGRAM_KIND, 1, 0]),
            None,
            "no entries"
        );
        // The builder stops before the datagram would exceed the limit.
        let mut big = Nack::default();
        let mut id = 0;
        while big.try_push(NackEntry {
            frame_id: id,
            indices: vec![1; 40],
        }) {
            id += 1;
        }
        assert!(big.encode().len() <= MAX_NACK_LEN);
        assert!(Nack::decode(&big.encode()).is_some());
    }

    #[test]
    fn resends_marked_packets_within_the_budget() {
        let mut w = ResendWindow::default();
        let data = vec![9u8; 20_000];
        let pkts = fragment(4, false, T, &data, 1200, Some(0.2));
        let k = PacketHeader::parse(&pkts[0]).unwrap().0.data_count as usize;
        w.insert(4, k, pkts.clone(), 0);
        let out = w.resend(&nack(4, &[2, 3]), 0);
        assert_eq!(out.len(), 2);
        let (h, body) = PacketHeader::parse(&out[0]).unwrap();
        let (orig, orig_body) = PacketHeader::parse(&pkts[2]).unwrap();
        assert!(h.resent && !orig.resent);
        assert_eq!((h.index, h.frame_id, body), (2, 4, orig_body));
        // Whole-frame request = the data shards only.
        let all = w.resend(&nack(4, &[]), 0);
        assert!(all.len() <= k && !all.is_empty());
        // Unknown frames and out-of-range indices are counted, not sent.
        assert!(w.resend(&nack(99, &[0]), 0).is_empty());
        assert!(w.resend(&nack(4, &[60_000]), 0).is_empty());
        assert!(w.stats.unknown >= 2);
    }

    #[test]
    fn budget_is_128_kib_per_second() {
        let mut w = ResendWindow::default();
        let pkts = fragment(1, false, T, &vec![1u8; 400_000], 1200, None);
        let k = pkts.len();
        w.insert(1, k, pkts, 0);
        // The burst (64 KiB) goes out at once, the rest is over budget.
        let first = w.resend(&nack(1, &[]), 0);
        let sent: usize = first.iter().map(Bytes::len).sum();
        assert!(
            sent <= RESEND_BURST_BYTES as usize && sent > 60_000,
            "{sent}"
        );
        assert!(w.stats.over_budget > 0);
        // One second later 128 KiB would be due, but the bucket holds only the burst.
        let again = w.resend(&nack(1, &[]), 1_000_000);
        let sent: usize = again.iter().map(Bytes::len).sum();
        assert!(
            sent <= RESEND_BURST_BYTES as usize && sent > 60_000,
            "{sent}"
        );
        // 100 ms later: ~12.8 KiB of tokens.
        let small = w.resend(&nack(1, &[]), 1_100_000);
        let sent: usize = small.iter().map(Bytes::len).sum();
        assert!((10_000..=14_000).contains(&sent), "{sent}");
    }

    #[test]
    fn window_keeps_three_seconds_and_8_mib() {
        let mut w = ResendWindow::default();
        for id in 0..10u32 {
            w.insert(
                id,
                1,
                vec![Bytes::from(vec![0u8; 10])],
                id as u64 * 1_000_000,
            );
        }
        // Frames older than 3 s (relative to the newest insert) are gone.
        assert_eq!(w.len(), 4);
        assert!(w.resend(&nack(0, &[0]), 9_000_000).is_empty());
        assert_eq!(w.resend(&nack(9, &[0]), 9_000_000).len(), 1);
        // Memory cap.
        let mut w = ResendWindow::default();
        for id in 0..100u32 {
            w.insert(id, 1, vec![Bytes::from(vec![0u8; 1 << 20])], 0);
        }
        assert!(w.bytes() <= RESEND_WINDOW_BYTES);
        assert_eq!(w.len(), 8);
        w.clear();
        assert!(w.is_empty() && w.bytes() == 0);
    }
}
