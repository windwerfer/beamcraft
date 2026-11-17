//! Video frames travel as unreliable QUIC datagrams. Each encoded frame is cut
//! into equal-size data shards (the last one is sent unpadded), optionally
//! followed by FEC parity shards ([`crate::fec`]). Every packet carries the
//! same header, so the receiver can reassemble frames, rebuild lost shards,
//! detect gaps and measure per-stage latency without extra round trips.

use crate::be;
use crate::control::QueueDelay;
use crate::delay::DelayMeter;
use crate::fec::{block_len, parity_count, recover_block, LinkStats};
use bytes::{BufMut, Bytes, BytesMut};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// First byte of every video packet (pointer datagrams start with 0xC1).
pub const WIRE_VERSION: u8 = 2;
pub const HEADER_LEN: usize = 44;
/// Data shards per block before a frame is split into more blocks.
pub const MAX_BLOCK_DATA: usize = 64;
const MAX_BLOCKS: usize = 255;
/// Bigger claimed frames are rejected (hostile input).
pub const MAX_FRAME_LEN: usize = 64 << 20;
const FLAG_KEYFRAME: u8 = 0b0000_0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTiming {
    pub capture_us: u64,
    pub encoded_us: u64,
    pub sent_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub keyframe: bool,
    pub frame_id: u32,
    /// `0..data_count` are data shards, `data_count..data_count+parity_count` parity.
    pub index: u16,
    pub data_count: u16,
    pub parity_count: u16,
    pub frame_len: u32,
    pub shard_len: u16,
    pub blocks: u8,
    pub timing: FrameTiming,
}

impl PacketHeader {
    fn write(&self, buf: &mut BytesMut) {
        buf.put_u8(WIRE_VERSION);
        buf.put_u8(if self.keyframe { FLAG_KEYFRAME } else { 0 });
        buf.put_u32(self.frame_id);
        buf.put_u16(self.index);
        buf.put_u16(self.data_count);
        buf.put_u16(self.parity_count);
        buf.put_u32(self.frame_len);
        buf.put_u16(self.shard_len);
        buf.put_u8(self.blocks);
        buf.put_u8(0);
        buf.put_u64(self.timing.capture_us);
        buf.put_u64(self.timing.encoded_us);
        buf.put_u64(self.timing.sent_us);
    }

    /// Parses and validates a packet. Inconsistent headers (from a
    /// broken or hostile sender) return `None`.
    pub fn parse(d: &[u8]) -> Option<(Self, &[u8])> {
        if *d.first()? != WIRE_VERSION {
            return None;
        }
        let h = Self {
            keyframe: be::<1>(d, 1).map(|b| b[0])? & FLAG_KEYFRAME != 0,
            frame_id: u32::from_be_bytes(be(d, 2)?),
            index: u16::from_be_bytes(be(d, 6)?),
            data_count: u16::from_be_bytes(be(d, 8)?),
            parity_count: u16::from_be_bytes(be(d, 10)?),
            frame_len: u32::from_be_bytes(be(d, 12)?),
            shard_len: u16::from_be_bytes(be(d, 16)?),
            blocks: be::<1>(d, 18)?[0],
            timing: FrameTiming {
                capture_us: u64::from_be_bytes(be(d, 20)?),
                encoded_us: u64::from_be_bytes(be(d, 28)?),
                sent_us: u64::from_be_bytes(be(d, 36)?),
            },
        };
        let payload = d.get(HEADER_LEN..)?;
        if !h.layout_ok() || payload.len() != h.payload_len(h.index as usize) {
            return None;
        }
        Some((h, payload))
    }

    fn layout_ok(&self) -> bool {
        let (k, sl, len) = (
            self.data_count as usize,
            self.shard_len as usize,
            self.frame_len as usize,
        );
        k >= 1
            && sl >= 2
            && sl % 2 == 0
            && self.blocks >= 1
            && self.blocks as usize <= k
            && (self.index as usize) < k + self.parity_count as usize
            && k + self.parity_count as usize <= u16::MAX as usize
            && len <= MAX_FRAME_LEN
            && len <= k * sl
            && (k == 1 || len > (k - 1) * sl)
    }

    /// Bytes carried by packet `index` (data shards are unpadded).
    pub fn payload_len(&self, index: usize) -> usize {
        let sl = self.shard_len as usize;
        if index >= self.data_count as usize {
            return sl;
        }
        sl.min((self.frame_len as usize).saturating_sub(index * sl))
    }

    /// Whether two packets describe the same frame layout.
    pub fn same_layout(&self, o: &Self) -> bool {
        (
            self.data_count,
            self.parity_count,
            self.frame_len,
            self.shard_len,
            self.blocks,
        ) == (
            o.data_count,
            o.parity_count,
            o.frame_len,
            o.shard_len,
            o.blocks,
        )
    }
}

/// Splits one encoded frame into data and parity packets no larger than
/// `max_datagram`. Data packets come first, in order (consecutive ones belong
/// to different blocks), then the parity packets, also interleaved.
/// `overhead` is parity / data ([`crate::fec::FecMode::overhead`]); `None`
/// sends no parity. Returns no packets for a frame over [`MAX_FRAME_LEN`].
pub fn fragment(
    frame_id: u32,
    keyframe: bool,
    timing: FrameTiming,
    payload: &[u8],
    max_datagram: usize,
    overhead: Option<f32>,
) -> Vec<Bytes> {
    let max_shard = (max_datagram.saturating_sub(HEADER_LEN) & !1).max(2);
    let shard_len = if payload.len() < max_shard {
        (payload.len().max(1) + 1) & !1
    } else {
        max_shard
    };
    let k = payload.len().div_ceil(shard_len).max(1);
    if k >= u16::MAX as usize || payload.len() > MAX_FRAME_LEN {
        return Vec::new(); // too large for the wire format; the caller drops the frame
    }
    let blocks = k.div_ceil(MAX_BLOCK_DATA).min(MAX_BLOCKS);
    let parity = overhead.map_or(0, |o| parity_count(k, blocks, o, keyframe));
    let mut header = PacketHeader {
        keyframe,
        frame_id,
        index: 0,
        data_count: k as u16,
        parity_count: parity as u16,
        frame_len: payload.len() as u32,
        shard_len: shard_len as u16,
        blocks: blocks as u8,
        timing,
    };
    let shard = |i: usize| -> Cow<'_, [u8]> {
        let start = (i * shard_len).min(payload.len());
        let end = (start + shard_len).min(payload.len());
        let s = payload.get(start..end).unwrap_or_default();
        if s.len() == shard_len {
            Cow::Borrowed(s)
        } else {
            let mut v = s.to_vec();
            v.resize(shard_len, 0);
            Cow::Owned(v)
        }
    };
    let packet = |h: &PacketHeader, body: &[u8]| {
        let mut buf = BytesMut::with_capacity(HEADER_LEN + body.len());
        h.write(&mut buf);
        buf.extend_from_slice(body);
        buf.freeze()
    };

    let mut out = Vec::with_capacity(k + parity);
    for i in 0..k {
        header.index = i as u16;
        let len = header.payload_len(i);
        let start = i * shard_len;
        out.push(packet(
            &header,
            payload.get(start..start + len).unwrap_or_default(),
        ));
    }
    // recovery[b][pos] is parity shard `b + pos * blocks`.
    let mut recovery: Vec<Vec<Vec<u8>>> = Vec::with_capacity(blocks);
    for b in 0..blocks {
        let m = block_len(parity, blocks, b);
        if m == 0 {
            recovery.push(Vec::new());
            continue;
        }
        let originals: Vec<Cow<'_, [u8]>> = (b..k).step_by(blocks).map(shard).collect();
        recovery
            .push(reed_solomon_simd::encode(originals.len(), m, &originals).unwrap_or_default());
    }
    for j in 0..parity {
        let Some(body) = recovery.get(j % blocks).and_then(|r| r.get(j / blocks)) else {
            continue;
        };
        header.index = (k + j) as u16;
        out.push(packet(&header, body));
    }
    out
}

#[derive(Debug, Clone)]
pub struct AssembledFrame {
    pub frame_id: u32,
    pub keyframe: bool,
    pub timing: FrameTiming,
    pub data: Vec<u8>,
    /// Data fragments the frame was split into (parity not counted).
    pub fragments: u16,
    pub first_fragment_us: u64,
    pub completed_us: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ReassemblyEvent {
    /// Number of frames skipped (never completed) before the delivered one.
    pub lost_frames: u32,
    /// Data fragments of this frame that were rebuilt from FEC parity.
    pub recovered: u32,
}

struct Partial {
    keyframe: bool,
    timing: FrameTiming,
    /// Layout of the first packet (all later ones must match).
    layout: PacketHeader,
    data: Vec<Option<Bytes>>,
    have_data: usize,
    parity: Vec<Option<Bytes>>,
    /// Shards (data + parity) received per block, and blocks already
    /// complete or rebuilt.
    block_have: Vec<u16>,
    block_done: Vec<bool>,
    recovered: u32,
    bytes: usize,
    first_us: u64,
}

impl Partial {
    fn new(h: &PacketHeader, now_us: u64) -> Self {
        let blocks = h.blocks as usize;
        Self {
            keyframe: h.keyframe,
            timing: h.timing,
            layout: *h,
            data: vec![None; h.data_count as usize],
            have_data: 0,
            parity: vec![None; h.parity_count as usize],
            block_have: vec![0; blocks],
            block_done: vec![false; blocks],
            recovered: 0,
            bytes: 0,
            first_us: now_us,
        }
    }

    /// Stores one packet; returns false for duplicates and mismatches.
    fn add(&mut self, h: &PacketHeader, body: Bytes) -> bool {
        if !self.layout.same_layout(h) {
            return false;
        }
        let (i, k) = (h.index as usize, self.data.len());
        let slot = if i < k {
            self.data.get_mut(i)
        } else {
            self.parity.get_mut(i - k)
        };
        let Some(slot) = slot else {
            return false;
        };
        if slot.is_some() {
            return false;
        }
        if i < k {
            self.have_data += 1;
            self.bytes += body.len();
        }
        *slot = Some(body);
        let blocks = self.layout.blocks as usize;
        let b = if i < k { i % blocks } else { (i - k) % blocks };
        if let Some(n) = self.block_have.get_mut(b) {
            *n = n.saturating_add(1);
        }
        self.try_recover(b);
        true
    }

    fn try_recover(&mut self, b: usize) {
        let blocks = self.layout.blocks as usize;
        let k = self.data.len();
        let kb = block_len(k, blocks, b);
        let have = self.block_have.get(b).copied().unwrap_or(0) as usize;
        let Some(done) = self.block_done.get_mut(b) else {
            return;
        };
        if *done || have < kb {
            return;
        }
        *done = true;
        let missing = (b..k)
            .step_by(blocks)
            .any(|i| self.data.get(i).is_some_and(Option::is_none));
        if !missing {
            return;
        }
        for (i, bytes) in
            recover_block(&self.layout, b, &self.data, &self.parity).unwrap_or_default()
        {
            if let Some(slot @ None) = self.data.get_mut(i) {
                self.bytes += bytes.len();
                *slot = Some(bytes);
                self.have_data += 1;
                self.recovered += 1;
            }
        }
    }

    fn complete(&self) -> bool {
        self.have_data == self.data.len()
    }
}

/// Partial frames kept at once (bounds memory against a flood of frame ids).
const MAX_PARTIAL: usize = 64;
/// Frames this far behind the newest are final for loss accounting.
const METER_LAG: u32 = 30;

/// Reassembles fragments into frames and delivers them strictly in order.
/// A frame that completes after a newer one has been delivered is dropped, and
/// any frames skipped over are reported as lost. Lost data shards are rebuilt
/// from parity as soon as enough of their block has arrived.
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    last_delivered: Option<u32>,
    max_age_us: u64,
    /// frame_id -> (packets sent, data packets, packets received).
    meter: BTreeMap<u32, (u32, u32, u32)>,
    newest: u32,
    stats: LinkStats,
    delay: DelayMeter,
}

impl Reassembler {
    pub fn new(max_age_us: u64) -> Self {
        Self {
            partial: BTreeMap::new(),
            last_delivered: None,
            max_age_us,
            meter: BTreeMap::new(),
            newest: 0,
            stats: LinkStats::default(),
            delay: DelayMeter::default(),
        }
    }

    pub fn push(
        &mut self,
        datagram: Bytes,
        now_us: u64,
    ) -> Option<(AssembledFrame, ReassemblyEvent)> {
        let (h, payload) = PacketHeader::parse(&datagram)?;
        let body = datagram.slice(datagram.len() - payload.len()..);
        self.meter(&h);
        self.delay.on_packet(h.frame_id, h.timing.sent_us, now_us);
        if let Some(last) = self.last_delivered {
            if h.frame_id <= last {
                return None;
            }
        }
        self.evict_stale(now_us);
        if !self.partial.contains_key(&h.frame_id) && self.partial.len() >= MAX_PARTIAL {
            self.partial.pop_first();
        }
        let frame_id = h.frame_id;
        let entry = self
            .partial
            .entry(frame_id)
            .or_insert_with(|| Partial::new(&h, now_us));
        let before = entry.recovered;
        if !entry.add(&h, body) {
            return None;
        }
        self.stats.recovered += (entry.recovered - before) as u64;
        if !entry.complete() {
            return None;
        }

        let done = self.partial.remove(&frame_id)?;
        let lost_frames = match self.last_delivered {
            Some(last) => frame_id - last - 1,
            None => 0,
        };
        self.stats.unrecoverable_frames += lost_frames as u64;
        self.partial.retain(|&id, _| id > frame_id);
        self.last_delivered = Some(frame_id);

        let mut data = Vec::with_capacity(done.bytes);
        for part in done.data.iter().flatten() {
            data.extend_from_slice(part);
        }
        Some((
            AssembledFrame {
                frame_id,
                keyframe: done.keyframe,
                timing: done.timing,
                data,
                fragments: done.data.len() as u16,
                first_fragment_us: done.first_us,
                completed_us: now_us,
            },
            ReassemblyEvent {
                lost_frames,
                recovered: done.recovered,
            },
        ))
    }

    pub fn pending(&self) -> usize {
        self.partial.len()
    }

    /// Cumulative packet counters (loss is counted once a frame is
    /// [`METER_LAG`] frames old, so late packets still count as received).
    pub fn link_stats(&self) -> LinkStats {
        self.stats
    }

    /// Queueing delay since the previous call (see [`crate::delay`]).
    pub fn take_delay(&mut self) -> Option<QueueDelay> {
        self.delay.take()
    }

    fn meter(&mut self, h: &PacketHeader) {
        let total = h.data_count as u32 + h.parity_count as u32;
        if h.frame_id.saturating_add(METER_LAG) < self.newest {
            return; // already finalized
        }
        let e = self
            .meter
            .entry(h.frame_id)
            .or_insert((total, h.data_count as u32, 0));
        e.2 = e.2.saturating_add(1);
        if h.frame_id > self.newest {
            self.newest = h.frame_id;
        }
        while let Some((&id, _)) = self.meter.first_key_value() {
            if id.saturating_add(METER_LAG) >= self.newest
                && self.meter.len() <= 4 * METER_LAG as usize
            {
                break;
            }
            let Some((_, (total, data, got))) = self.meter.pop_first() else {
                break;
            };
            self.stats.expected += total as u64;
            self.stats.received += got.min(total) as u64;
            self.stats.data_expected += data as u64;
            self.stats.parity_expected += (total - data) as u64;
        }
    }

    fn evict_stale(&mut self, now_us: u64) {
        let max_age = self.max_age_us;
        self.partial
            .retain(|_, p| now_us.saturating_sub(p.first_us) <= max_age);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: FrameTiming = FrameTiming {
        capture_us: 1,
        encoded_us: 2,
        sent_us: 3,
    };

    fn payload(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn roundtrip_out_of_order() {
        let data = payload(10_000);
        let mut frags = fragment(7, true, T, &data, 1200, None);
        // FEC off: data packets only, in the one packet format.
        let (h, _) = PacketHeader::parse(&frags[0]).unwrap();
        assert_eq!((h.parity_count, frags.len()), (0, h.data_count as usize));
        assert!(frags
            .iter()
            .all(|p| p[0] == WIRE_VERSION && p.len() <= 1200));
        frags.reverse();
        let mut r = Reassembler::new(1_000_000);
        let mut out = None;
        for f in frags {
            if let Some(x) = r.push(f, 10) {
                out = Some(x);
            }
        }
        let (frame, ev) = out.expect("frame assembled");
        assert_eq!(ev.recovered, 0);
        assert_eq!(frame.data, data);
        assert!(frame.keyframe);
        assert_eq!(frame.timing, T);
        assert_eq!(ev.lost_frames, 0);
    }

    #[test]
    fn detects_loss_and_drops_late() {
        let mut r = Reassembler::new(1_000_000);
        let f1 = fragment(1, false, T, &payload(3000), 1200, None);
        let f2 = fragment(2, false, T, &payload(3000), 1200, None);
        let f3 = fragment(3, false, T, &payload(100), 1200, None);
        for f in &f1 {
            r.push(f.clone(), 0);
        }
        r.push(f2[0].clone(), 0);
        let (frame, ev) = r.push(f3[0].clone(), 0).unwrap();
        assert_eq!(frame.frame_id, 3);
        assert_eq!(ev.lost_frames, 1);
        assert!(r.push(f2[1].clone(), 0).is_none());
        assert_eq!(r.pending(), 0);
        assert_eq!(r.link_stats().unrecoverable_frames, 1);
    }

    #[test]
    fn empty_payload_is_one_fragment() {
        let frags = fragment(0, false, T, &[], 1200, None);
        assert_eq!(frags.len(), 1);
        let mut r = Reassembler::new(1);
        assert!(r.push(frags[0].clone(), 0).unwrap().0.data.is_empty());
    }
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    #[test]
    fn link_stats_count_loss_after_lag() {
        let t = FrameTiming {
            capture_us: 0,
            encoded_us: 0,
            sent_us: 0,
        };
        let mut r = Reassembler::new(u64::MAX);
        for id in 0..100u32 {
            let pkts = fragment(id, false, t, &vec![1u8; 11_000], 1200, Some(0.2));
            for (i, p) in pkts.iter().enumerate() {
                if i != 3 {
                    r.push(p.clone(), 0);
                }
            }
        }
        let s = r.link_stats();
        // 10 data + 2 parity per frame, one lost each; 69 frames are final.
        assert_eq!((s.expected, s.received), (69 * 12, 69 * 11));
        assert_eq!((s.data_expected, s.parity_expected), (69 * 10, 69 * 2));
        assert_eq!(s.recovered, 100);
        assert_eq!(s.unrecoverable_frames, 0);
        assert!((s.loss_pct() - 100.0 / 12.0).abs() < 0.01);
        assert!((s.overhead_pct() - 20.0).abs() < 0.01);
    }
}
