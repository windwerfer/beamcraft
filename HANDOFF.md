# HANDOFF

## Goal
Item 0c (unsafe reduction), second part: document and lint every unsafe site in mirrorwire-media.

## Done
- b2dc005: SAFETY comments on all 28 unsafe blocks/impls in media (encode.rs, decode.rs, hw.rs); `#![deny(clippy::undocumented_unsafe_blocks)]` in media lib.rs; portal.rs reviewed (stays); roadmap updated. No new unsafe. Breaks: none.
- Earlier: 112a9e3 (app: no unsafe except yuv_glow).

## Unsafe inventory (file: why)
- net, proto: none (forbidden). app: yuv_glow.rs only (glow GL calls are unsafe fns).
- media: encode.rs, decode.rs, hw.rs (FFmpeg/VAAPI FFI, `unsafe impl Send`), source/portal.rs (`BorrowedFd::borrow_raw` on PipeWire's raw fd; no safe API). android gpu.rs (EGL/GL FFI).

## Decisions
None new (follows D15).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) still pending.
- Owner: is an ASan CI job for media tests wanted (needs nightly toolchain)?

## Verify
cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; scripts/loopback-test.sh 10 --encoder x264; gh run list --branch main --limit 3

## Written at commit
b2dc005

## Next step
0c: move the FFmpeg unsafe into one small safe module (hw.rs-style wrappers for codec-context options, filter helpers, DRM mapping), then a Decoder trait with our own frame type. Or 0b.3 xtask ci gate.
