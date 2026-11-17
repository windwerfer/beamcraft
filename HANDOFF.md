# HANDOFF

## Goal
Roadmap item 5 remainder: the Android viewer sends the bottleneck verdict too.

## Done
- Classifier moved from mirrorwire-app to `mirrorwire-proto/src/bottleneck.rs` (move-only commit a98ac4e). Android `session.rs` runs it once a second (in the pipeline-HUD tick) and sends `Control::Bottleneck` on change (d50eb04). Decoder kind = label prefix `SOFTWARE`. No Android HUD line for the hint text yet.
- Gates: `cargo xtask ci` (loopback p95 15 ms), workspace clippy, app --all-features clippy, Android clippy (cargo ndk, as CI). CI green on d50eb04. Not run on a device.
- Earlier: presenter shows the verdict (protocol 7), viewer HUD, lints, 0b, 0c.

## Decisions
- One classifier in proto for both viewers (decisions.md 2026-11-17). Rejected: copy in the Android crate, new crate.

## Open questions
- Owner: SonarCloud switch (SONAR_CI_ANALYSIS) pending.
- Owner: ASan CI job for media tests (needs nightly)?
- Is 60 ms right for CI runners? Loopback p95 here is ~18 ms; tighten after the first CI run.

## Verify
cargo xtask ci; gh run list --branch main --limit 3

## Written at commit
see `git log -1`

## Next step
Roadmap 6 (extend mode, testable parts: mode setting plumbing, AvailableSourceTypes check), then 4 (system FFmpeg). Optional: Android HUD line with the hint text.
