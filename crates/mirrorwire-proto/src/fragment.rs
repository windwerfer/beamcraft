//! Video frames travel as unreliable QUIC datagrams. Each encoded frame is cut
//! into equal-size data shards (the last one is sent unpadded), optionally
//! followed by FEC parity shards ([`crate::fec`]). Every packet carries the
//! same header, so the receiver can reassemble frames, rebuild lost shards,
//! detect gaps and measure per-stage latency without extra round trips.

use crate::be;
use crate::control::QueueDelay;
use crate::crc::crc32;
use crate::delay::DelayMeter;
use crate::fec::{block_len, parity_count, recover_block, LinkStats};
use crate::nack::{Nack, NackEntry};
use bytes::{BufMut, Bytes, BytesMut};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// First byte of every video packet (pointer datagrams start with 0xC1).
pub const WIRE_VERSION: u8 = 3;
pub const HEADER_LEN: usize = 48;
/// Data shards per block before a frame is split into more blocks.
pub const MAX_BLOCK_DATA: usize = 64;
const MAX_BLOCKS: usize = 255;
/// Bigger claimed frames are rejected (hostile input).
pub const MAX_FRAME_LEN: usize = 64 << 20;
const FLAG_KEYFRAME: u8 = 0b0000_0001;
/// Set on a packet the presenter sends again after a NACK ([`crate::nack`]).
const FLAG_RESENT: u8 = 0b0000_0010;

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
    /// A retransmission: not a first transmission, so it says nothing about loss or delay.
    pub resent: bool,
    /// CRC-32 of the whole encoded frame ([`crate::crc`]), the same in every packet of it.
    pub frame_crc: u32,
    pub timing: FrameTiming,
}

impl PacketHeader {
    fn write(&self, buf: &mut BytesMut) {
        buf.put_u8(WIRE_VERSION);
        let flags = [(self.keyframe, FLAG_KEYFRAME), (self.resent, FLAG_RESENT)];
        buf.put_u8(
            flags
                .iter()
                .filter(|(on, _)| *on)
                .fold(0, |a, (_, f)| a | f),
        );
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
        buf.put_u32(self.frame_crc);
    }

    /// Parses and validates a packet. Inconsistent headers (from a
    /// broken or hostile sender) return `None`.
    pub fn parse(d: &[u8]) -> Option<(Self, &[u8])> {
        if *d.first()? != WIRE_VERSION {
            return None;
        }
        let h = Self {
            keyframe: be::<1>(d, 1)?[0] & FLAG_KEYFRAME != 0,
            resent: be::<1>(d, 1)?[0] & FLAG_RESENT != 0,
            frame_id: u32::from_be_bytes(be(d, 2)?),
            index: u16::from_be_bytes(be(d, 6)?),
            data_count: u16::from_be_bytes(be(d, 8)?),
            parity_count: u16::from_be_bytes(be(d, 10)?),
            frame_len: u32::from_be_bytes(be(d, 12)?),
            shard_len: u16::from_be_bytes(be(d, 16)?),
            blocks: be::<1>(d, 18)?[0],
            frame_crc: u32::from_be_bytes(be(d, 44)?),
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
            self.frame_crc,
        ) == (
            o.data_count,
            o.parity_count,
            o.frame_len,
            o.shard_len,
            o.blocks,
            o.frame_crc,
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
        resent: false,
        frame_crc: crc32(payload),
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

/// A copy of `packet` with the resent flag set (the original stays in the resend window).
pub fn mark_resent(packet: &Bytes) -> Bytes {
    let mut v = packet.to_vec();
    if let Some(flags) = v.get_mut(1) {
        *flags |= FLAG_RESENT;
    }
    Bytes::from(v)
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
    /// When the last new packet arrived, and whether the last packet the presenter sends for
    /// the frame (the final parity shard) is among those received.
    last_us: u64,
    got_last: bool,
    /// NACKs sent for this frame and when the last one went out.
    nack_rounds: u8,
    last_nack_us: u64,
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
            last_us: now_us,
            got_last: false,
            nack_rounds: 0,
            last_nack_us: 0,
        }
    }

    /// Stores one packet; returns false for duplicates and mismatches.
    fn add(&mut self, h: &PacketHeader, body: Bytes, now_us: u64) -> bool {
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
        self.last_us = now_us;
        self.got_last |= i + 1 == k + self.parity.len();
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

    /// The fewest data shards that, once resent, let every unfinished block complete: per
    /// block, as many missing data shards as the block still lacks of its `k` (parity that
    /// did arrive already counts).
    fn shards_to_nack(&self) -> Vec<u16> {
        let (k, blocks) = (self.data.len(), self.layout.blocks as usize);
        let mut out = Vec::new();
        for b in 0..blocks {
            if self.block_done.get(b).copied().unwrap_or(true) {
                continue;
            }
            let have = self.block_have.get(b).copied().unwrap_or(0) as usize;
            let need = block_len(k, blocks, b).saturating_sub(have);
            out.extend(
                (b..k)
                    .step_by(blocks)
                    .filter(|&i| self.data.get(i).is_some_and(Option::is_none))
                    .take(need)
                    .map(|i| i as u16),
            );
        }
        out
    }
}

/// Partial frames kept at once (bounds memory against a flood of frame ids).
const MAX_PARTIAL: usize = 64;
/// Frames this far behind the newest are final for loss accounting.
const METER_LAG: u32 = 30;
/// The newest frame is NACKed without waiting for a newer one once it has been quiet this long
/// (its tail was lost and no later frame is coming soon, e.g. on a static screen).
const TAIL_QUIET_US: u64 = 15_000;
/// NACK rounds per frame before the viewer gives up on resends.
const MAX_NACK_ROUNDS: u8 = 3;
/// Frames past the head looked at when building a NACK.
const NACK_SPAN: u32 = 64;

/// What the viewer's NACKs achieved (cumulative).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NackStats {
    /// Fragments asked for (a whole-frame request counts as one).
    pub requested: u64,
    /// Resent packets that arrived, and those of them that filled a gap.
    pub resent_received: u64,
    pub resent_useful: u64,
}

/// Reassembles fragments into frames and delivers them strictly in order.
/// Lost data shards are rebuilt from parity as soon as enough of their block has arrived;
/// what parity cannot cover is requested again ([`Reassembler::take_nacks`]). A frame that is
/// still incomplete when a newer one is ready holds the newer ones back for
/// [`Reassembler::hold_us`] (about two round trips); after that it is given up, reported as lost
/// and the caller asks for a keyframe. Late fragments of a frame already given up are dropped.
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    last_delivered: Option<u32>,
    max_age_us: u64,
    /// frame_id -> (packets sent, data packets, packets received).
    meter: BTreeMap<u32, (u32, u32, u32)>,
    newest: u32,
    stats: LinkStats,
    delay: DelayMeter,
    rtt_us: u64,
    /// Frames given up since the last delivered one.
    pending_lost: u32,
    /// Whole-frame NACKs: frame_id -> (rounds, last time).
    absent: BTreeMap<u32, (u8, u64)>,
    nacks: NackStats,
    corrupt_frames: u64,
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
            rtt_us: 0,
            pending_lost: 0,
            absent: BTreeMap::new(),
            nacks: NackStats::default(),
            corrupt_frames: 0,
        }
    }

    /// The path's round-trip time, which sets how long a missing frame is waited for.
    pub fn set_rtt_us(&mut self, rtt_us: u64) {
        self.rtt_us = rtt_us;
    }

    /// How long a frame that blocks newer ones is waited for: two round trips plus 4 ms
    /// (6 ms to 100 ms).
    pub fn hold_us(&self) -> u64 {
        self.rtt_us
            .saturating_mul(2)
            .saturating_add(4_000)
            .clamp(6_000, 100_000)
    }

    /// Delay before a fragment is requested again: one round trip plus 2 ms.
    fn retry_us(&self) -> u64 {
        self.rtt_us.saturating_add(2_000)
    }

    /// Adds a packet and returns the next frame that can be delivered, if any. Call
    /// [`Reassembler::poll`] until it returns `None` for the frames queued behind it.
    pub fn push(
        &mut self,
        datagram: Bytes,
        now_us: u64,
    ) -> Option<(AssembledFrame, ReassemblyEvent)> {
        let (h, payload) = PacketHeader::parse(&datagram)?;
        let body = datagram.slice(datagram.len() - payload.len()..);
        if h.resent {
            // A resend is not a first transmission: it must not count as loss or delay.
            self.nacks.resent_received += 1;
        } else {
            self.stats.bytes_received += datagram.len() as u64;
            self.meter(&h);
            self.delay.on_packet(h.frame_id, h.timing.sent_us, now_us);
        }
        if let Some(last) = self.last_delivered {
            if h.frame_id <= last {
                return None;
            }
        }
        self.evict_stale(now_us);
        if !self.partial.contains_key(&h.frame_id) && self.partial.len() >= MAX_PARTIAL {
            self.partial.pop_first();
        }
        let entry = self
            .partial
            .entry(h.frame_id)
            .or_insert_with(|| Partial::new(&h, now_us));
        let before = entry.recovered;
        if !entry.add(&h, body, now_us) {
            return None;
        }
        self.nacks.resent_useful += u64::from(h.resent);
        self.stats.recovered += (entry.recovered - before) as u64;
        self.poll(now_us)
    }

    /// Delivers the next frame in order, if it is complete or the frame before it has been
    /// given up. Also the place where time passes: call it when no packet arrives.
    pub fn poll(&mut self, now_us: u64) -> Option<(AssembledFrame, ReassemblyEvent)> {
        loop {
            let newest = *self.partial.keys().next_back()?;
            let head = self.head()?;
            if self.partial.get(&head).is_some_and(Partial::complete) {
                let done = self.partial.remove(&head)?;
                self.last_delivered = Some(head);
                let want_crc = done.layout.frame_crc;
                let lost_frames = self.pending_lost;
                let (frame, event) = finish(head, done, lost_frames, now_us);
                if crc32(&frame.data) != want_crc {
                    // Reassembly or FEC rebuilt something wrong: never feed it to the decoder.
                    // Handled like a lost frame (the caller asks for a keyframe).
                    self.corrupt_frames += 1;
                    self.pending_lost += 1;
                    self.stats.unrecoverable_frames += 1;
                    continue;
                }
                self.pending_lost = 0;
                return Some((frame, event));
            }
            if head >= newest {
                return None; // nothing newer yet: the frame is still arriving
            }
            // How long a newer frame has been waiting behind the head, measured from the
            // arrival of its first packet (not from when the head was first examined: a run
            // of missing frames must be given up together, or after an outage the viewer
            // gives up one frame per hold time, slower than the presenter sends new ones,
            // and never delivers again).
            let (&next, blocker) = self
                .partial
                .range(head.wrapping_add(u32::from(self.partial.contains_key(&head)))..)
                .next()?;
            if now_us.saturating_sub(blocker.first_us) < self.hold_us() {
                return None;
            }
            // Resends did not arrive in time: give up the head, and with it every frame id
            // before the next one that has anything.
            let skipped = if self.partial.remove(&head).is_some() {
                1
            } else {
                next.wrapping_sub(head)
            };
            self.last_delivered = Some(head.wrapping_add(skipped - 1));
            self.pending_lost = self.pending_lost.saturating_add(skipped);
            self.stats.unrecoverable_frames += u64::from(skipped);
        }
    }

    /// The next frame the delivery waits for.
    fn head(&self) -> Option<u32> {
        match self.last_delivered {
            Some(l) => Some(l.wrapping_add(1)),
            None => self.partial.keys().next().copied(),
        }
    }

    /// How long a caller may sleep before it must call [`Reassembler::poll`] and
    /// [`Reassembler::take_nacks`] again (timers run only while a frame is incomplete).
    pub fn wake_after(&self) -> std::time::Duration {
        if self.partial.is_empty() {
            std::time::Duration::from_millis(100)
        } else {
            std::time::Duration::from_millis(2)
        }
    }

    /// The fragments to ask the presenter for now: what FEC cannot rebuild of frames that
    /// are settled (a newer frame has begun, or their last packet arrived, or they went
    /// quiet), and whole frames of which nothing arrived. Each fragment is asked for again
    /// after one round trip, up to three times.
    pub fn take_nacks(&mut self, now_us: u64) -> Option<Nack> {
        let newest = *self.partial.keys().next_back()?;
        let head = self.head()?;
        let retry = self.retry_us();
        self.absent.retain(|&id, _| id >= head);
        let mut nack = Nack::default();
        for id in head..=newest.min(head.saturating_add(NACK_SPAN)) {
            let due = |rounds: u8, last: u64| {
                rounds < MAX_NACK_ROUNDS && (rounds == 0 || now_us.saturating_sub(last) >= retry)
            };
            if let Some(p) = self.partial.get_mut(&id) {
                let settled =
                    id < newest || p.got_last || now_us.saturating_sub(p.last_us) >= TAIL_QUIET_US;
                if p.complete() || !settled || !due(p.nack_rounds, p.last_nack_us) {
                    continue;
                }
                let indices = p.shards_to_nack();
                let n = indices.len() as u64;
                if n == 0 {
                    continue;
                }
                if !nack.try_push(NackEntry {
                    frame_id: id,
                    indices,
                }) {
                    break;
                }
                p.nack_rounds += 1;
                p.last_nack_us = now_us;
                self.nacks.requested += n;
            } else if id < newest {
                let (rounds, last) = *self.absent.entry(id).or_insert((0, 0));
                if !due(rounds, last) {
                    continue;
                }
                if !nack.try_push(NackEntry {
                    frame_id: id,
                    indices: Vec::new(),
                }) {
                    break;
                }
                self.absent.insert(id, (rounds + 1, now_us));
                self.nacks.requested += 1;
            }
        }
        (!nack.is_empty()).then_some(nack)
    }

    /// Frames that completed but failed their CRC and were dropped (cumulative). Should stay 0:
    /// QUIC authenticates every packet, so a failure means a reassembly or FEC bug.
    pub fn corrupt_frames(&self) -> u64 {
        self.corrupt_frames
    }

    pub fn nack_stats(&self) -> NackStats {
        self.nacks
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

fn finish(
    frame_id: u32,
    done: Partial,
    lost_frames: u32,
    now_us: u64,
) -> (AssembledFrame, ReassemblyEvent) {
    let mut data = Vec::with_capacity(done.bytes);
    for part in done.data.iter().flatten() {
        data.extend_from_slice(part);
    }
    (
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
    )
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

    /// Regression (hardware run 2026-11-18): after an outage of N whole frames the head was
    /// given up one frame per hold time, slower than new frames arrive, so nothing was ever
    /// delivered again ("in 0.0 fps" for 40 s).
    #[test]
    fn missing_run_is_skipped_at_once() {
        let mut r = Reassembler::new(1_000_000);
        for f in fragment(1, true, T, &payload(100), 1200, None) {
            assert!(r.push(f, 0).is_some());
        }
        // Frames 2..=100 never arrive; 101.. arrive complete every 16 ms.
        let mut delivered = None;
        let mut now = 1_000_000;
        for id in 101..140u32 {
            for f in fragment(id, false, T, &payload(100), 1200, None) {
                if let Some(x) = r.push(f, now) {
                    delivered.get_or_insert((x.0.frame_id, x.1.lost_frames, now));
                }
            }
            if delivered.is_some() {
                break;
            }
            now += 16_000;
        }
        let (id, lost, at) = delivered.expect("delivery resumes");
        assert!(
            at <= 1_000_000 + r.hold_us() + 16_000,
            "resumed too late: {at}"
        );
        assert_eq!(id, 101);
        assert_eq!(lost, 99);
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
        // Frame 3 is complete but waits behind the incomplete frame 2 (resends may still come).
        assert!(r.push(f3[0].clone(), 0).is_none());
        assert!(r.poll(r.hold_us() - 1).is_none());
        let (frame, ev) = r.poll(r.hold_us()).unwrap();
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

#[cfg(test)]
mod nack_tests {
    use super::*;
    use crate::nack::ResendWindow;

    const T: FrameTiming = FrameTiming {
        capture_us: 0,
        encoded_us: 0,
        sent_us: 0,
    };

    fn payload(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8 ^ seed).collect()
    }

    /// Frames 1..=n of ~10 packets each, no FEC; `lose(frame, index)` drops a packet.
    fn feed(
        r: &mut Reassembler,
        win: &mut ResendWindow,
        frames: &[u32],
        lose: impl Fn(u32, usize) -> bool,
        now: u64,
    ) -> Vec<u32> {
        let mut got = Vec::new();
        for &id in frames {
            let pkts = fragment(id, false, T, &payload(11_000, id as u8), 1200, None);
            let k = pkts.len();
            win.insert(id, k, pkts.clone(), now);
            for (i, p) in pkts.into_iter().enumerate() {
                if !lose(id, i) {
                    if let Some((f, _)) = r.push(p, now) {
                        got.push(f.frame_id);
                    }
                }
            }
        }
        got
    }

    #[test]
    fn a_frame_failing_its_crc_is_never_delivered() {
        let mut r = Reassembler::new(u64::MAX);
        let good = fragment(1, true, T, &payload(5_000, 1), 1200, None);
        let mut bad = fragment(2, false, T, &payload(5_000, 2), 1200, None);
        // One body byte flipped in a way no header check can see.
        let mut v = bad[1].to_vec();
        v[HEADER_LEN + 3] ^= 0x40;
        bad[1] = Bytes::from(v);
        let next = fragment(3, false, T, &payload(100, 3), 1200, None);
        let mut ids = Vec::new();
        for p in good.into_iter().chain(bad).chain(next) {
            ids.extend(r.push(p, 0).map(|(f, e)| (f.frame_id, e.lost_frames)));
        }
        // Frame 2 is dropped as lost, frame 3 follows it and reports the loss.
        assert_eq!(ids, [(1, 0), (3, 1)]);
        assert_eq!(r.corrupt_frames(), 1);
        assert_eq!(r.link_stats().unrecoverable_frames, 1);
    }

    #[test]
    fn crc_is_in_every_packet_and_must_agree() {
        let pkts = fragment(1, false, T, &payload(5_000, 1), 1200, Some(0.2));
        let crc = PacketHeader::parse(&pkts[0]).unwrap().0.frame_crc;
        assert_eq!(crc, crate::crc::crc32(&payload(5_000, 1)));
        assert!(pkts
            .iter()
            .all(|p| PacketHeader::parse(p).unwrap().0.frame_crc == crc));
        // A packet claiming another CRC for the same frame is ignored (layout mismatch).
        let mut r = Reassembler::new(u64::MAX);
        r.push(pkts[0].clone(), 0);
        let mut v = pkts[1].to_vec();
        v[47] ^= 1;
        assert!(r.push(Bytes::from(v), 0).is_none());
        assert_eq!(r.pending(), 1);
    }

    #[test]
    fn a_lost_fragment_is_resent_and_no_frame_is_lost() {
        let mut r = Reassembler::new(u64::MAX);
        let mut win = ResendWindow::default();
        let got = feed(
            &mut r,
            &mut win,
            &[1, 2, 3],
            |f, i| f == 2 && (i == 3 || i == 7),
            0,
        );
        assert_eq!(got, [1], "2 is incomplete, 3 waits behind it");
        let nack = r.take_nacks(1_000).expect("a NACK");
        assert_eq!(nack.entries.len(), 1);
        assert_eq!(nack.entries[0].frame_id, 2);
        assert_eq!(nack.entries[0].indices, [3, 7]);
        // Not asked twice within a round trip.
        assert!(r.take_nacks(1_100).is_none());
        let resent = win.resend(&nack, 1_000);
        assert_eq!(resent.len(), 2);
        let mut delivered = Vec::new();
        for p in resent {
            if let Some((f, ev)) = r.push(p, 1_500) {
                assert_eq!(ev.lost_frames, 0);
                delivered.push(f);
            }
            while let Some((f, ev)) = r.poll(1_500) {
                assert_eq!(ev.lost_frames, 0);
                delivered.push(f);
            }
        }
        let ids: Vec<u32> = delivered.iter().map(|f| f.frame_id).collect();
        assert_eq!(ids, [2, 3]);
        assert_eq!(delivered[0].data, payload(11_000, 2));
        assert_eq!(r.link_stats().unrecoverable_frames, 0);
        let s = r.nack_stats();
        assert_eq!((s.requested, s.resent_received, s.resent_useful), (2, 2, 2));
    }

    #[test]
    fn resent_packets_do_not_count_as_received() {
        let mut r = Reassembler::new(u64::MAX);
        let mut win = ResendWindow::default();
        let frames: Vec<u32> = (1..=60).collect();
        feed(&mut r, &mut win, &frames, |f, i| f == 5 && i == 2, 0);
        let nack = r.take_nacks(0).unwrap();
        for p in win.resend(&nack, 0) {
            r.push(p, 0);
        }
        while r.poll(0).is_some() {}
        let s = r.link_stats();
        // 29 frames are final (60 - METER_LAG + 1); frame 5 lost one of its ~10 packets on air.
        assert_eq!(s.expected - s.received, 1, "{s:?}");
        assert_eq!(s.unrecoverable_frames, 0);
    }

    #[test]
    fn missing_resend_gives_up_after_the_hold_time() {
        let mut r = Reassembler::new(u64::MAX);
        r.set_rtt_us(10_000);
        assert_eq!(r.hold_us(), 24_000);
        let mut win = ResendWindow::default();
        let got = feed(&mut r, &mut win, &[1, 2, 3], |f, i| f == 2 && i == 4, 0);
        assert_eq!(got, [1]);
        assert!(r.take_nacks(0).is_some());
        assert!(r.poll(23_999).is_none());
        // One retry after RTT + 2 ms, three rounds in all, then silence.
        assert!(r.take_nacks(11_999).is_none());
        assert!(r.take_nacks(12_000).is_some());
        assert!(r.take_nacks(30_000).is_some());
        assert!(r.take_nacks(60_000).is_none(), "three rounds at most");
        // The resend never came: frame 2 is lost, frame 3 is delivered.
        let (f, ev) = r.poll(24_000).expect("hold expired");
        assert_eq!((f.frame_id, ev.lost_frames), (3, 1));
        assert_eq!(r.link_stats().unrecoverable_frames, 1);
        // A late resend of the given-up frame is ignored.
        let pkts = fragment(2, false, T, &payload(11_000, 2), 1200, None);
        assert!(r.push(mark_resent(&pkts[4]), 25_000).is_none());
    }

    #[test]
    fn a_frame_of_which_nothing_arrived_is_requested_whole() {
        let mut r = Reassembler::new(u64::MAX);
        let mut win = ResendWindow::default();
        let got = feed(&mut r, &mut win, &[1, 2, 3], |f, _| f == 2, 0);
        assert_eq!(got, [1]);
        let nack = r.take_nacks(0).unwrap();
        assert_eq!(nack.entries[0].frame_id, 2);
        assert!(nack.entries[0].indices.is_empty());
        let mut ids = Vec::new();
        for p in win.resend(&nack, 0) {
            ids.extend(r.push(p, 0).map(|(f, _)| f.frame_id));
            while let Some((f, _)) = r.poll(0) {
                ids.push(f.frame_id);
            }
        }
        assert_eq!(ids, [2, 3]);
    }

    #[test]
    fn the_newest_frame_is_requested_when_it_goes_quiet() {
        let mut r = Reassembler::new(u64::MAX);
        let mut win = ResendWindow::default();
        // No parity, last packet of the only frame lost: nothing newer will arrive.
        feed(&mut r, &mut win, &[1], |_, i| i == 9, 0);
        assert!(r.take_nacks(1_000).is_none(), "may still be in flight");
        let nack = r.take_nacks(TAIL_QUIET_US).unwrap();
        assert_eq!(nack.entries[0].indices, [9]);
    }

    #[test]
    fn parity_that_arrived_reduces_what_is_requested() {
        let mut r = Reassembler::new(u64::MAX);
        let pkts = fragment(1, false, T, &payload(11_000, 1), 1200, Some(0.2));
        let k = PacketHeader::parse(&pkts[0]).unwrap().0.data_count as usize;
        let parity = pkts.len() - k;
        assert!(parity >= 2);
        // Lose three data shards: parity covers `parity` of them, the rest is requested.
        for (i, p) in pkts.iter().enumerate() {
            if i != 1 && i != 2 && i != 3 {
                r.push(p.clone(), 0);
            }
        }
        let nack = r.take_nacks(0).unwrap();
        assert_eq!(nack.requested(), 3 - parity.min(3));
    }
}
