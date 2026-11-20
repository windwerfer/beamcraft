//! RTP (RFC 3550) header parsing and a small reorder buffer (sans-IO, never panics on input).

/// RTP payload type for MPEG-2 TS (RFC 3551), what WFD sources use.
pub const PT_MP2T: u8 = 33;

/// A parsed RTP packet; `payload` borrows from the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub marker: bool,
    pub payload_type: u8,
    pub seq: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

/// Why a datagram was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpError {
    TooShort,
    BadVersion,
    BadLengths,
}

/// Parse one RTP datagram (CSRC list, header extension and padding are skipped).
pub fn parse(d: &[u8]) -> Result<RtpPacket<'_>, RtpError> {
    let Some(&[b0, b1, s0, s1, t0, t1, t2, t3, c0, c1, c2, c3]) = d.first_chunk::<12>() else {
        return Err(RtpError::TooShort);
    };
    if b0 >> 6 != 2 {
        return Err(RtpError::BadVersion);
    }
    let mut start = 12 + usize::from(b0 & 0x0F) * 4;
    if b0 & 0x10 != 0 {
        let Some(&[_, _, l0, l1]) = d.get(start..).and_then(|s| s.first_chunk::<4>()) else {
            return Err(RtpError::BadLengths);
        };
        start += 4 + usize::from(u16::from_be_bytes([l0, l1])) * 4;
    }
    let mut end = d.len();
    if b0 & 0x20 != 0 {
        let n = usize::from(*d.last().ok_or(RtpError::BadLengths)?);
        end = end.checked_sub(n).ok_or(RtpError::BadLengths)?;
    }
    let payload = d.get(start..end).ok_or(RtpError::BadLengths)?;
    Ok(RtpPacket {
        marker: b1 & 0x80 != 0,
        payload_type: b1 & 0x7F,
        seq: u16::from_be_bytes([s0, s1]),
        timestamp: u32::from_be_bytes([t0, t1, t2, t3]),
        ssrc: u32::from_be_bytes([c0, c1, c2, c3]),
        payload,
    })
}

/// Output of [`Reorder::push`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// Payloads now in order.
    pub payloads: Vec<Vec<u8>>,
    /// Packets given up on (a gap was skipped): the stream has a discontinuity.
    pub lost: u32,
}

/// Reorder buffer: holds out-of-order packets up to `depth`; when full, skips the gap.
#[derive(Debug)]
pub struct Reorder {
    depth: usize,
    next: Option<u16>,
    held: Vec<(u16, Vec<u8>)>,
    /// Late or duplicate packets dropped.
    pub dropped_late: u64,
}

impl Reorder {
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
            next: None,
            held: Vec::new(),
            dropped_late: 0,
        }
    }

    /// Forget the sequence state (new SSRC / session).
    pub fn reset(&mut self) {
        self.next = None;
        self.held.clear();
    }

    pub fn push(&mut self, seq: u16, payload: &[u8]) -> Released {
        let mut out = Released::default();
        let next = *self.next.get_or_insert(seq);
        let ahead = seq.wrapping_sub(next);
        if ahead >= 0x8000 || self.held.iter().any(|(s, _)| *s == seq) {
            self.dropped_late += 1; // already delivered/skipped, or a duplicate
            return out;
        }
        if ahead == 0 {
            self.next = Some(next.wrapping_add(1));
            out.payloads.push(payload.to_vec());
        } else {
            self.held.push((seq, payload.to_vec()));
            if self.held.len() > self.depth {
                // Give up on the gap: jump to the oldest held packet.
                let base = self.next.unwrap_or(next);
                let oldest = self
                    .held
                    .iter()
                    .map(|(s, _)| s.wrapping_sub(base))
                    .min()
                    .unwrap_or(0);
                out.lost += u32::from(oldest);
                self.next = Some(base.wrapping_add(oldest));
            }
        }
        self.drain(&mut out);
        out
    }

    fn drain(&mut self, out: &mut Released) {
        while let Some(next) = self.next {
            match self.held.iter().position(|(s, _)| *s == next) {
                Some(i) => {
                    out.payloads.push(self.held.swap_remove(i).1);
                    self.next = Some(next.wrapping_add(1));
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x80, PT_MP2T | 0x80];
        v.extend_from_slice(&seq.to_be_bytes());
        v.extend_from_slice(&[0, 0, 0x10, 0, 1, 2, 3, 4]);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_header() {
        let d = pkt(7, b"abc");
        let p = parse(&d).unwrap();
        assert_eq!(
            (p.seq, p.payload_type, p.marker, p.ssrc),
            (7, 33, true, 0x01020304)
        );
        assert_eq!(p.timestamp, 0x1000);
        assert_eq!(p.payload, b"abc");
    }

    #[test]
    fn rejects_garbage_without_panic() {
        assert_eq!(parse(&[0x80; 5]), Err(RtpError::TooShort));
        assert_eq!(parse(&[0x40; 12]), Err(RtpError::BadVersion));
        let mut d = pkt(1, b"x");
        d[0] |= 0x10; // extension flagged but absent
        assert_eq!(parse(&d), Err(RtpError::BadLengths));
        let mut d = pkt(1, b"x");
        d[0] |= 0x20;
        *d.last_mut().unwrap() = 200; // padding longer than packet
        assert_eq!(parse(&d), Err(RtpError::BadLengths));
        let mut d = pkt(1, b"x");
        d[0] |= 0x0F; // 15 CSRCs but packet is short
        assert_eq!(parse(&d), Err(RtpError::BadLengths));
    }

    #[test]
    fn extension_and_padding_are_skipped() {
        let mut d = vec![0xB0, 33, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        d.extend_from_slice(&[0xBE, 0xDE, 0, 1, 9, 9, 9, 9]); // ext, 1 word
        d.extend_from_slice(&[b'h', b'i', 0, 0, 3]); // 3 padding bytes incl. count
        assert_eq!(parse(&d).unwrap().payload, b"hi");
    }

    #[test]
    fn in_order_passes_through() {
        let mut r = Reorder::new(4);
        for s in 100..105u16 {
            let o = r.push(s, &[s as u8]);
            assert_eq!(o.payloads, vec![vec![s as u8]]);
            assert_eq!(o.lost, 0);
        }
    }

    #[test]
    fn reorders_within_depth() {
        let mut r = Reorder::new(4);
        r.push(1, b"1");
        assert!(r.push(3, b"3").payloads.is_empty());
        let o = r.push(2, b"2");
        assert_eq!(o.payloads, vec![b"2".to_vec(), b"3".to_vec()]);
        assert_eq!(o.lost, 0);
    }

    #[test]
    fn gap_is_skipped_and_counted() {
        let mut r = Reorder::new(2);
        r.push(1, b"1");
        assert!(r.push(4, b"4").payloads.is_empty());
        assert!(r.push(5, b"5").payloads.is_empty());
        let o = r.push(6, b"6"); // 3 held > depth 2: give up on 2 and 3
        assert_eq!(o.lost, 2);
        assert_eq!(
            o.payloads,
            vec![b"4".to_vec(), b"5".to_vec(), b"6".to_vec()]
        );
        // The skipped packet arriving late is dropped.
        assert!(r.push(2, b"2").payloads.is_empty());
        assert_eq!(r.dropped_late, 1);
    }

    #[test]
    fn sequence_wraps() {
        let mut r = Reorder::new(4);
        r.push(65534, b"a");
        assert!(r.push(0, b"c").payloads.is_empty());
        let o = r.push(65535, b"b");
        assert_eq!(o.payloads, vec![b"b".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn duplicates_are_dropped() {
        let mut r = Reorder::new(4);
        r.push(1, b"1");
        assert!(r.push(1, b"1").payloads.is_empty());
        r.push(3, b"3");
        assert!(r.push(3, b"3").payloads.is_empty());
        assert_eq!(r.dropped_late, 2);
    }
}
