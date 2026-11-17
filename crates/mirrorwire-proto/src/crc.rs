//! CRC-32 (IEEE 802.3, the zlib/PNG polynomial) of a whole encoded frame, carried in every video
//! packet header so the viewer can refuse a frame that reassembly or FEC rebuilt wrongly.
//! QUIC already authenticates each packet on the wire; this catches our own bugs. Slicing-by-8,
//! no dependency (`crc32fast` would do the same with SIMD; a frame is ~30 KB, a keyframe ~300 KB,
//! and the table version needs well under a millisecond for even that).

const POLY: u32 = 0xEDB8_8320;

// Evaluated at compile time: an index out of range would fail the build, not panic at runtime.
#[allow(clippy::indexing_slicing)]
const fn tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut s = 1;
        while s < 8 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            s += 1;
        }
        i += 1;
    }
    t
}

static TABLES: [[u32; 256]; 8] = tables();

/// CRC-32 of `data` (`crc32(b"123456789") == 0xCBF4_3926`).
// Indexes 256-entry tables with values masked or shifted into 0..256, and 8-byte arrays at
// 0..8: no index can be out of range.
#[allow(clippy::indexing_slicing)]
pub fn crc32(data: &[u8]) -> u32 {
    let t = &TABLES;
    let mut crc = !0u32;
    let (chunks, rest) = data.as_chunks::<8>();
    for c in chunks {
        let lo = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) ^ crc;
        let hi = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
        crc = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in rest {
        crc = (crc >> 8) ^ t[0][((crc ^ b as u32) & 0xff) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
    }

    #[test]
    fn slicing_matches_bytewise_and_detects_flips() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + i / 3) as u8).collect();
        let bytewise = |d: &[u8]| {
            let mut c = !0u32;
            for &b in d {
                c = (c >> 8) ^ TABLES[0][((c ^ b as u32) & 0xff) as usize];
            }
            !c
        };
        for len in [0, 1, 7, 8, 9, 63, 1000] {
            assert_eq!(crc32(&data[..len]), bytewise(&data[..len]), "{len}");
        }
        let good = crc32(&data);
        for i in [0, 1, 500, 999] {
            let mut bad = data.clone();
            bad[i] ^= 0x10;
            assert_ne!(crc32(&bad), good, "{i}");
        }
    }

    /// `cargo test -p mirrorwire-proto --release crc::tests::throughput -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn throughput() {
        let data = vec![0x5au8; 300_000];
        let t = std::time::Instant::now();
        let mut x = 0u32;
        for _ in 0..1000 {
            x ^= crc32(&data);
        }
        let s = t.elapsed().as_secs_f64();
        println!("crc32: {:.0} MB/s ({x})", 300_000.0 * 1000.0 / s / 1e6);
    }
}
