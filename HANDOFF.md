# HANDOFF

## Goal
v0.1.0 released: https://github.com/windwerfer/mirrorwire/releases/tag/v0.1.0 (dmg, apk, AppImage, flatpak, windows zip, CHANGELOG notes). Tag run's release job failed (no *.zip: Windows artifact arrives as a folder); release was created by hand from run 44753111691 artifacts, build.yml now zips the folder (untested until the next tag). Parked Miracast roadmap entry added.

## Earlier goal
Owner order 2026-11-19 12:29: no backwards compatibility until 1.x; v0.1.0 release prep.

## Done
- Audit: the code was already nearly compat-free (exact PROTOCOL_VERSION match). Removed: `--renderer glow|opengl` aliases and the MIRRORWIRE_RENDERER glow/opengl values, an "older viewers" doc line, an "older builds can read a newer file" settings doc.
- Rule recorded: AGENTS.md design-priority paragraph and decisions.md D6 (no "Breaks:" bookkeeping; note changes in CHANGELOG.md). protocol.md history remarks removed.
- CHANGELOG.md with a 0.1.0 section. build.yml release job: checks out CHANGELOG.md, uses the `## <version>` section as release notes (fails if missing); it already uploads dmg/AppImage/flatpak/apk/zip.
- Gates: cargo xtask ci + Android clippy green.

## Kept on purpose (runtime fallbacks / unsure)
VAAPI->x264, combined portal session->ScreenCast, low_power retry; mDNS list still shows peers with other/missing `v` marked "older/newer: update" (that is the version-mismatch message, not compat); unknown settings fields ignored by serde.

## Open questions
- Hardware checks pending (pen/touch, camera scan, Extend sizing, presenter remote on other desktops); Windows/macOS viewers never run on real machines.
- Owner: craft-libs decoder.rs:532 upstream report, then drop continue-on-error in fuzz.yml.
- Manager: tag v0.1.0 after CI on main is green (the release job needs the CHANGELOG section).

## Verify
cargo xtask ci; cargo ndk -t arm64-v8a --platform 26 clippy --release --locked -p mirrorwire-android -- -D warnings; gh run list --branch main --limit 3

## Next step
NOTHING_LEFT after CI is green and the manager tags v0.1.0.
