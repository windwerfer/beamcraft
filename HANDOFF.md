# HANDOFF

## Goal
Make GitHub Actions on main green (AppImage, Flatpak, SonarCloud).

## Done
- dc89684 SonarCloud CI scan runs only if repo variable `SONAR_CI_ANALYSIS` = `true`; else skips with a notice.
- 579a4de AppImage: real cause was a stale static-FFmpeg cache (.pc prefix from the old repo path); cache key now has the repo name, plus an early check step. No system -dev packages needed.
- 82393e4, 08ff850 Flatpak: image runtimes can't be updated/removed on the container overlay ("Invalid cross-device link"); the job now wipes /var/lib/flatpak with rm first (mount is not available).
- bebd7c6 decisions.md / README / testing.md.
- Result: Build run 44066686432 and CI all jobs success (SonarCloud skipped by design).

## Decisions
decisions.md "CI scan switch" and "CI build fixes". No unsafe, no protocol change.

## Open questions
- Owner: to get the CI Sonar scan (clippy + coverage), turn off Automatic Analysis in SonarCloud, then set variable SONAR_CI_ANALYSIS=true. Until then Automatic Analysis scans.
- Flatpak job now downloads runtimes every run (slower); revisit when the image is fixed.

## Verify
gh run list --branch main --limit 5; gh run view <Build run id>

## Written at commit
08ff850

## Next step
Owner flips the SonarCloud setting + variable (optional).
