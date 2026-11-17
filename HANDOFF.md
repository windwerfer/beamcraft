# HANDOFF

## Goal
Item 0b.3: a `cargo xtask ci` gate with a mechanical crate-layer check.

## Done
- 6b5584e: std-only `xtask/` crate (`cargo xtask ci [--no-loopback]`, `cargo xtask layers`), `.cargo/config.toml` alias, CI runs the layer check, AGENTS.md/testing.md/roadmap.md updated. No new unsafe. Breaks: none.
- Earlier: b2dc005 (media SAFETY comments + deny lint), 112a9e3 (app unsafe removed).

## Unsafe inventory (file: why)
- net, proto: none (forbidden). app: yuv_glow.rs only (glow GL calls). media: encode.rs, decode.rs, hw.rs (FFmpeg/VAAPI FFI), source/portal.rs (`BorrowedFd::borrow_raw`). android gpu.rs (EGL/GL FFI).

## Decisions
None new (xtask is std-only, no new dependency).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) still pending.
- Owner: ASan CI job for media tests (needs nightly)?

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
6b5584e

## Next step
0b.1 rest: `[workspace.lints]` + clippy.toml, forbid(unsafe_code) in film/android helpers; or 0b.4 TOML perf budgets; or 0c: FFmpeg unsafe into one safe module.
