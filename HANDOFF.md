# HANDOFF

## Goal
NACK resend budget scales with the current ABR target: ~15 %, never below 128 KiB/s, capped.

## Done
- 0c9fcc7 `ResendWindow::set_target_kbps`, `resend_rate_bps` (15 %, 128 KiB/s..2 MiB/s, burst = 0.5 s of rate, min 64 KiB); presenter feeds `LinkControl::budget_kbps` before each NACK; 2 new unit tests.
- 2285236 docs: decisions.md, protocol.md §3d, testing.md.
- Testing numbers commit + this file: see git log.

## Decisions
decisions.md "resend budget scales with the bitrate". No wire change, no protocol bump, no unsafe.

## Open questions
- 15 % / 2 MiB/s cap are unmeasured on real Wi-Fi. Loopback never exceeded the old budget (0 over budget).

## Verify
cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace;
scripts/loopback-test.sh 20 --encoder x264 -- --simulate-loss 10

## Written at commit
2285236 (plus the testing.md numbers commit after it)

## Next step
Measure on real Wi-Fi (scripts/android-wifi-run.sh) and compare "over budget" counts in the presenter log.
