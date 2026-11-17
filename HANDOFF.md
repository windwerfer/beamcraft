# HANDOFF

## Goal
Item 0c/0b (unsafe reduction), first part: remove unsafe from logging in the app crate.

## Done
- 112a9e3: applog.rs `libc::isatty` -> `std::io::IsTerminal`; `libc` dep removed; mirrorwire-app is `#![deny(unsafe_code)]` with a single `allow` on `mod yuv_glow`. No new unsafe. Breaks: none.

## Unsafe inventory (file: why)
- net, proto: none (net forbids). app: only yuv_glow.rs (glow GL calls are unsafe fns; no safe glow API).
- media: encode.rs, decode.rs, hw.rs (FFmpeg/VAAPI FFI), source/portal.rs (one site, not yet reviewed). android gpu.rs (EGL/GL FFI).

## Decisions
No decisions.md change (follows D15).

## Open questions
- Owner: SonarCloud switch from the previous handoff (SONAR_CI_ANALYSIS) still pending.

## Verify
cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; scripts/loopback-test.sh 10 --encoder x264; gh run list --branch main --limit 3

## Written at commit
112a9e3

## Next step
0c: look at the unsafe in media source/portal.rs for a safe replacement; add SAFETY comments + clippy::undocumented_unsafe_blocks in media; then a Decoder trait with our own frame type. Alternatively 0b.3 xtask ci gate.
