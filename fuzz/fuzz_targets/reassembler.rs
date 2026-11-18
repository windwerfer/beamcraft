//! A stream of hostile datagrams (split at 0xFF) into the reassembler, with time moving on.
#![no_main]
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use mirrorwire_proto::fragment::Reassembler;

fuzz_target!(|data: &[u8]| {
    let mut r = Reassembler::new(200_000);
    let mut now = 0u64;
    for d in data.split(|b| *b == 0xFF).take(256) {
        now += 1_000;
        let _ = r.push(Bytes::copy_from_slice(d), now);
        while r.poll(now).is_some() {}
        let _ = r.take_nacks(now);
    }
});
