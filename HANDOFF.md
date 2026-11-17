# HANDOFF

## Goal
Item 0b.1 (part): compiler-enforced `unsafe_code` policy per crate plus `clippy.toml`.

## Done
- f454021: `forbid(unsafe_code)` in proto, net, film, xtask; `deny` in media, app, android with `#[allow(unsafe_code)]` only on decode, encode, hw, source::portal, yuv_glow, android gpu. `clippy.toml` (unwrap/expect/panic/indexing allowed in tests). No new unsafe. Breaks: none.
- 3fbc176: decisions.md (D15 follow-up) and roadmap.md.
- Earlier: 6b5584e xtask ci gate, b2dc005 media SAFETY comments, 112a9e3 app unsafe removed.

## Unsafe inventory (file: why)
- net, proto, film, xtask: none (forbidden). app: yuv_glow.rs (glow GL). media: encode.rs, decode.rs, hw.rs (FFmpeg/VAAPI FFI), source/portal.rs (`BorrowedFd::borrow_raw`). android gpu.rs (EGL/GL FFI, not compiled on this box).

## Decisions
D15 follow-up in decisions.md (lint policy; widening the allow list needs the owner).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
3fbc176 (plus this file)

## Next step
0b.4 TOML perf budgets; or 0c: move FFmpeg unsafe into one small safe module; or workspace-wide clippy unwrap lints (many sites).
