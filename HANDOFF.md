# HANDOFF

## Goal
Item 0b.5, fifth part: split the big files (presenter.rs).

## Done
- 7897ce7: `presenter.rs` -> `presenter/mod.rs` + `presenter/status.rs` (PresentPhase, EncodeHud, PresentStatus; pure move). No unsafe added. Breaks: none. roadmap.md updated. GUI window not run (no display).
- Earlier: 6c14e7b encode/rc.rs, 4bd0440 encode/vaapi.rs, ef1e7cb config split, 3e0c0d5 needs_new_encoder, eabf95a perf budgets, f454021 unsafe_code lint, 6b5584e xtask ci.

## Decisions
- Pure code move, no decisions.md change.

## Open questions
- BLOCKER on this build box (2026-11-17 session): no pkg-config, FFmpeg/PipeWire/VA/dbus/wayland dev packages, no root/sudo, and the Debian mirror's apt lists are unsigned/500, so `cargo clippy/test` cannot build mirrorwire-app/media here. Rust stable and the aarch64-linux-android target were installed via rustup, but there is no NDK. The CaptureCtl/CaptureStatus move (presenter/capture.rs, fields pub(super)) was prepared and discarded unverified; redo it where the gates can run (it is mechanical: lines "Live capture settings" .. `impl Default for CaptureStatus` of presenter/mod.rs).
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~18 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
61cb02b (no code change; build box still lacks pkg-config/FFmpeg/PipeWire dev packages, sudo and NDK, re-checked 2026-11-17)

## Next step
presenter/mod.rs still ~1250 lines: move PresentArgs (needs pub(super) fields) and CaptureCtl to their own files; gui.rs (1321) split; or 0c: FFmpeg unsafe behind one safe module + Decoder trait.
