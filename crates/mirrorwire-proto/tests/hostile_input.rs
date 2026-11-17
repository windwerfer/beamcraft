//! Fuzz-style tests: every parser that sees network bytes must return an error or `None` for
//! hostile input, never panic (AGENTS.md §1.9). A deterministic xorshift generator keeps the
//! tests reproducible and dependency-free; see the 2026-11-17 entry in decisions.md for why this is not cargo-fuzz.

use bytes::Bytes;
use mirrorwire_proto::control::{hello_protocol, Control};
use mirrorwire_proto::cursor::{CursorImage, PointerUpdate};
use mirrorwire_proto::fragment::{fragment, FrameTiming, PacketHeader, Reassembler, HEADER_LEN};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Flips, overwrites, truncates and extends a valid message.
fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(5) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = rng.next() as u8;
            }
            2 => v.truncate(rng.below(v.len() + 1)),
            3 => {
                let n = rng.below(64);
                v.extend(rng.bytes(n));
            }
            _ if !v.is_empty() => {
                // Set a whole byte to an extreme value (lengths, counts).
                let i = rng.below(v.len());
                v[i] = [0, 1, 0x7f, 0x80, 0xff][rng.below(5)];
            }
            _ => {}
        }
    }
    v
}

const T: FrameTiming = FrameTiming {
    capture_us: u64::MAX,
    encoded_us: 0,
    sent_us: u64::MAX,
};

#[test]
fn packet_parser_survives_random_and_mutated_bytes() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let valid = fragment(5, true, T, &rng.bytes(5000), 1200, Some(0.3));
    for _ in 0..20_000 {
        let n = rng.below(HEADER_LEN * 2);
        let _ = PacketHeader::parse(&rng.bytes(n));
        let base = &valid[rng.below(valid.len())];
        let _ = PacketHeader::parse(&mutate(&mut rng, base));
    }
    // Every prefix of a valid packet is rejected or accepted without a panic.
    for cut in 0..valid[0].len() {
        let _ = PacketHeader::parse(&valid[0][..cut]);
    }
}

#[test]
fn reassembler_survives_a_hostile_flood() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut pool = Vec::new();
    for id in 0..6u32 {
        let len = [0, 1, 700, 3000, 20_000, 90_000][id as usize];
        pool.extend(fragment(
            id,
            id % 2 == 0,
            T,
            &rng.bytes(len),
            1200,
            Some(0.25),
        ));
    }
    let mut r = Reassembler::new(50_000);
    for step in 0..30_000u64 {
        let base = &pool[rng.below(pool.len())];
        let pkt = if rng.below(3) == 0 {
            base.to_vec()
        } else {
            mutate(&mut rng, base)
        };
        let _ = r.push(Bytes::from(pkt), step * 100);
        let _ = r.link_stats();
        let _ = r.take_delay();
    }
}

#[test]
fn reassembler_handles_corrupt_bodies_with_valid_headers() {
    // Valid layout, garbage bodies: FEC recovery must not panic.
    let mut rng = Rng(42);
    let data = rng.bytes(30_000);
    let pkts = fragment(1, false, T, &data, 1200, Some(0.5));
    for round in 0..50 {
        let mut r = Reassembler::new(u64::MAX);
        for p in &pkts {
            if rng.below(4) == 0 {
                continue;
            }
            let mut v = p.to_vec();
            if rng.below(3) == 0 {
                for b in v.iter_mut().skip(HEADER_LEN) {
                    *b = rng.next() as u8;
                }
            }
            let _ = r.push(Bytes::from(v), round);
        }
    }
}

#[test]
fn oversized_frame_yields_no_packets() {
    let big = vec![0u8; mirrorwire_proto::fragment::MAX_FRAME_LEN + 1];
    assert!(fragment(1, false, T, &big, 1200, None).is_empty());
}

#[test]
fn cursor_parsers_survive_hostile_bytes() {
    let mut rng = Rng(7);
    let pointer = PointerUpdate {
        visible: true,
        tool: mirrorwire_proto::cursor::PointerTool::Cursor,
        seq: 1,
        x: -5,
        y: i32::MAX,
        frame_w: 1920,
        frame_h: 1080,
        image_id: 3,
        capture_us: 9,
    }
    .encode();
    let image = CursorImage {
        id: 1,
        width: 4,
        height: 4,
        hotspot_x: 1,
        hotspot_y: 1,
        rgba: vec![7; 64],
    }
    .encode();
    for _ in 0..20_000 {
        let n = rng.below(80);
        let _ = PointerUpdate::decode(&rng.bytes(n));
        let _ = CursorImage::decode(&rng.bytes(n));
        let _ = PointerUpdate::decode(&mutate(&mut rng, &pointer));
        let _ = CursorImage::decode(&mutate(&mut rng, &image));
    }
    for cut in 0..image.len() {
        let _ = CursorImage::decode(&image[..cut]);
    }
}

#[test]
fn control_parsers_survive_hostile_bytes() {
    let mut rng = Rng(99);
    let msgs = [
        Control::Hello {
            protocol: 3,
            name: "x".into(),
            pairing_code: Some("123456".into()),
        },
        Control::Ping { t_viewer_us: 1 },
        Control::Bye,
    ];
    for _ in 0..20_000 {
        let n = rng.below(120);
        let junk = rng.bytes(n);
        let _ = Control::decode_body(&junk);
        let _ = hello_protocol(&junk);
        let _ = Control::check_len(rng.next() as u32);
        let m = &msgs[rng.below(msgs.len())].encode();
        let body = mutate(&mut rng, &m[4..]);
        let _ = Control::decode_body(&body);
        let _ = hello_protocol(&body);
    }
}
