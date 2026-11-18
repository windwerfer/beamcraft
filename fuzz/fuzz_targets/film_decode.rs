//! Feeds arbitrary bytes, split into access units at a 0xFF-prefixed marker, to the pure-Rust
//! H.264 decoder. It must return errors, never panic or crash. libFuzzer's panic hook aborts
//! before the adapter's `catch_unwind`, so a decoder panic is a finding here.
#![no_main]
use libfuzzer_sys::fuzz_target;
use mirrorwire_film::FilmDecoder;

fuzz_target!(|data: &[u8]| {
    let mut dec = FilmDecoder::default();
    for au in data.split(|b| *b == 0xFF).take(16) {
        let _ = dec.decode(au);
    }
});
