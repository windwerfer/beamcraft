# HANDOFF

## Goal
Roadmap item 6, testable part: Mirror/Extend capture mode.

## Done
- `CaptureMode` (media/source/mod.rs); portal picks MONITOR or VIRTUAL via `pick_source_type` (falls back to Mirror with a note when VIRTUAL is not offered); one restore token per mode. Plumbing: `--mode`, `MIRRORWIRE_MODE`, settings.json `mode`, Mode box in the Quality panel (re-opens capture). Unit tests for pick, precedence, restart, settings roundtrip.
- Gates: `cargo xtask ci`, app --all-features clippy, Android clippy (cargo ndk, as CI). Not run on a real portal or a device.

## Decisions
- decisions.md 2026-11-17 (extend mode): per-mode token, Mirror = MONITOR only. Rejected: shared token, viewer size in hello (protocol bump).

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here ~18 ms; tighten after the first CI run.
- Extend: should the viewer send its display size in hello (protocol 8)? Flatpak portal-version check for VIRTUAL untested.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Roadmap 4 (system FFmpeg). Optional: Android HUD line for the bottleneck hint; viewer size in hello for Extend.
