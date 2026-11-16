# Testing and verification

## 1. Gates

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/loopback-test.sh 10 --encoder x264
cargo audit
cargo deny --all-features --locked check advisories bans licenses sources
```

`cargo-audit` and `cargo-deny` are not part of the Rust toolchain; install them
with `cargo install cargo-audit cargo-deny` (or `taiki-e/install-action` in CI).

Build dependencies (Fedora and Ubuntu package lists) are in the README and in
`.github/workflows/ci.yml`. A headless box without a GPU or a portal can run all of these gates.

**Formatting note.** `main` is not yet `cargo fmt`-clean (the code was written to a wider line
width and there is no `rustfmt.toml`), and CI does not check formatting. Until the owner picks
a width and one formatting-only commit lands, don't reformat files you aren't otherwise
changing: format only the files you edit (`rustfmt --edition 2021 path/to/file.rs`) and keep
the diff readable. See [roadmap.md](roadmap.md) "Known debt".

## 2. Test layers

| Kind | Where | What it proves |
|---|---|---|
| Unit | `#[cfg(test)]` in `beamcraft-proto` | header encode/parse, fragmentation and reassembly (loss, reordering, late fragments), control framing, clock offset, stats |
| Unit | `#[cfg(test)]` in `beamcraft-net` | identity load/create, URI round trip (including a non-default port) and rejection of bad fingerprints, QR generation, port settings precedence, firewall guide text, ufw rule parsing |
| Unit (FilmCraft) | `beamcraft-media/src/filmcraft.rs`, feature `filmcraft` | FilmCraft output via the real `Encoder` decodes with FFmpeg, markers read back, keyframe on request; an `#[ignore]` x264-vs-FilmCraft benchmark |
| Unit (app) | `#[cfg(test)]` in `beamcraft-app` | CLI parsing, capture/FEC preference precedence, process log, and `presenter::tests::test_source_streams_to_an_in_process_viewer`: test-pattern presenter (x264) → QUIC → viewer with 1 % loss in one process, live FEC/bitrate/cursor changes, markers read back (needs FFmpeg with libx264; part of the coverage report) |
| Unit (firewall) | `beamcraft-net/src/firewall.rs` | hint and notice for every firewalld/ufw state, a fake `Firewalld` proving the startup probe makes no privileged call, one click = one authorization and cancel = no further prompts (D14), D-Bus error classification, zone merging, open-port messages (the D-Bus/process glue is `firewall/system.rs`) |
| Unit (rate) | `beamcraft-proto/src/rate.rs`, `fec.rs`, `beamcraft-media/src/pipeline.rs` | AIMD controller (cuts on queue/loss/backlog, floor 3 Mbit/s, ceiling = user setting, learned random-loss baseline), closed-loop sims with a FIFO bottleneck, auto FEC caps 35 %/60 %, token-bucket bandwidth simulator (viewer `--simulate-bandwidth-kbps`), x264 bitrate change live without a keyframe, VAAPI reopen rate-limited |
| Loopback (end to end) | `scripts/loopback-test.sh` | test pattern → encoder → QUIC (pinned TLS, pairing) → reassembly → decoder, on localhost, with latency stats |
| Real hardware | the owner's T480s + iMac | portal capture, VAAPI, Wi-Fi, OpenGL on a non-Metal Mac (only he can run this) |

### The oracle: the frame marker

The test pattern stamps its frame number into the image as black/white blocks
(`pattern::stamp_marker`). The headless viewer reads it back from the *decoded* picture
(`pattern::read_marker`) and counts mismatches. That proves the frame shown is the frame sent,
independent of our own bookkeeping. Keep this oracle working: any change to scaling, colour
conversion or the decoder must still read the markers back.

Use FFmpeg's `ffprobe`/`ffmpeg` only as an **external** check (for example, decoding a dumped
bitstream to prove an encoder's output is standard H.264). Never make the app depend on a
command-line ffmpeg at runtime.

## 3. Measuring latency and quality

The loopback and every viewer print one stats line per second, and the presenter prints its own:

- viewer: `fps, Mbit/s, rtt, ms mean/p95/max:` per stage
- presenter: `fps, Mbit/s, keyframes, mtu, ms mean/p95/max:`

Stages: `capture_to_encoded` (presenter: capture, colour conversion/scaling, encode),
`encoded_to_sent`, `network` (clock-offset corrected), `reassembly`, `decode`, `convert`
(YUV→RGBA, GUI only), `capture_to_decoded` (end to end). The GUI overlay adds
`capture->texture`.

Rules (adapted from FilmCraft's "report progress honestly"):

- **Before/after on the same machine, same build profile, same arguments,** at least 10 s. Quote
  mean and p95 of `capture_to_decoded`, plus fps and Mbit/s. State the machine (CPU, GPU or "VM,
  no GPU") and that it was loopback.
- **Name what was measured vs estimated.** A VM result says nothing about VAAPI on UHD 620 or
  decode on the iMac. Write "untested on target" when that's the truth.
- Use `--simulate-loss <percent>` on the viewer to test loss recovery (add
  `--simulate-burst <mean packets>` for Wi-Fi-like bursts); quote decoded frames, lost frames,
  keyframe requests and the `fec overhead … packet loss …` line. Compare `--fec auto` against
  `--fec off` (= the old behaviour) on the presenter.
- Quality: compare at the same bitrate and resolution. Native resolution with no scaling is the
  baseline; a sharpness claim needs a photo/screenshot of text at 100 % or a PSNR/SSIM number
  from a decoded dump.

### 3.0 Forward error correction under simulated loss (box, 2026-11-13)

Agent box (8 vCPU Xeon VM, no GPU), loopback, release build, x264 1920×1080@60 test pattern,
40 Mbit/s, 10 s per run, loss simulated on the viewer
(`scripts/loopback-test.sh 10 --encoder x264 --fec MODE -- --simulate-loss L [--simulate-burst B]`).
"main" is commit 1ea1523 (no FEC). "fec off" is this build with `--fec off` (same wire format as
main, used for the bursty runs, which main's simulator can't do). Latency is
`capture_to_decoded` mean / p95 in ms over decoded frames; the max is dominated by x264 stalls
on the VM (`capture_to_encoded`), not the network.

| Loss | main: frames (lost, kf req) | fec off: frames | fec auto: frames (lost, kf req) | fec auto overhead | fec auto latency mean / p95 |
|---|---|---|---|---|---|
| 0 % | 599 (0, 0), 8.45 / 10.45 ms | 594–600, 9.2–9.5 / 11.5–11.8 ms | 602–603 (0, 0) | 12 % | 8.80–8.90 / 11.3–11.5 |
| 2 % random | 1 (366, 74) | 9 (357, 75) | 600–601 (0, 0) | 16–17 % | 8.8–9.0 / 10.4–10.9 |
| 5 % random | 0 (517, 26) | 0 (503, 27) | 592–597 (0–1, 1–2) | 25 % | 9.5–9.6 / 11.0–12.6 |
| 2 % bursts of 4 | — | 172 (159, 77) | 570 (10, 9) | 31 % | 9.2 / 11.1 |
| 5 % bursts of 4 | — | 40 (279, 78) | 562 (16, 12) | 40 % | 9.4 / 11.7 |
| 5 % bursts of 8 | — | — | 554 (17, 15) | 49 % | 9.1 / 11.8 |

Without FEC, almost no frame of a 40 Mbit/s 1080p stream survives 2 % loss: every frame has
~60 packets and a keyframe ~400, so one loss per frame is likely and the keyframe asked for to
recover is lost too. With auto FEC random loss is fully covered; bursty loss still costs a
frame now and then (each lost delta frame waits for a keyframe, since there is no intra
refresh), and the auto boost raises the parity until that mostly stops. Untested on real Wi-Fi.

### 3.1 FilmCraft vs x264 (box, 2026-11-13)

Machine: the agent box, 8 vCPU Intel Xeon VM, no GPU, release build, so this says nothing about
the T480s (untested on target). Two measurements:

Encoder-only benchmark (synthetic gradient + moving pattern, 120 frames, CBR 15 Mbit/s, Y-PSNR
against a BT.601 reference, so the ceiling is ~50 dB):
`BEAMCRAFT_BENCH_SIZE=1920x1080 cargo test --release -p beamcraft-media --features filmcraft -- --ignored bench_ --nocapture`

| Encoder | Size | encode mean / p95 | max fps | Mbit/s @60 | Y-PSNR |
|---|---|---|---|---|---|
| libx264 | 1920×1080 | 2.57 / 2.98 ms | ~389 | 14.4 | 42.10 dB |
| filmcraft-h264enc | 1920×1080 | 10.31 / 14.94 ms | ~97 | 14.6 | 41.71 dB |
| libx264 | 2000×1000 | 2.65 / 3.16 ms | ~377 | 14.3 | 41.87 dB |
| filmcraft-h264enc | 2000×1000 | 11.97 / 15.46 ms | ~84 | 14.7 | 41.54 dB |

End-to-end loopback, 15 s, test pattern at 60 fps, all frame markers read back, 0 mismatches
(`BEAMCRAFT_FEATURES=filmcraft scripts/loopback-test.sh 15 --encoder filmcraft --width W --height H --bitrate-kbps K`):

| Encoder | Size, bitrate | fps | Mbit/s | capture_to_encoded mean / p95 | capture_to_decoded mean / p95 |
|---|---|---|---|---|---|
| x264 | 1920×1080, 30 Mbit/s | 59.8 | 28.7 | 6.37 / 7.95 ms | 7.81 / 9.71 ms |
| filmcraft | 1920×1080, 30 Mbit/s | 59.9 | 21.0 | 14.67 / 19.23 ms | 15.74 / 20.82 ms |
| x264 | 2000×1000, 15 Mbit/s | 59.7 | 14.8 | 6.41 / 8.11 ms | 7.84 / 10.12 ms |
| filmcraft | 2000×1000, 15 Mbit/s | 59.9 | 15.0 | 15.61 / 20.13 ms | 17.13 / 22.70 ms |

Reading: FilmCraft keeps 60 fps at these sizes on 8 cores with similar quality per bit, but
costs ~8–9 ms more per frame than x264 (ultrafast/zerolatency) and much more CPU. At 30 Mbit/s
it undershot the target (21 Mbit/s) on this easy content. On the 4-core T480s expect it to be
slower still; VAAPI remains the default there.

Useful runs:

```sh
scripts/loopback-test.sh 30 --encoder x264 --width 1920 --height 1080 --bitrate-kbps 30000
BEAMCRAFT_FEATURES=filmcraft scripts/loopback-test.sh 15 --encoder filmcraft --width 1920 --height 1080 --bitrate-kbps 30000
scripts/loopback-test.sh 10 --intra-refresh -- --simulate-loss 0.5
```

### 3.2 Adaptive bitrate under a simulated bottleneck (box, 2026-11-13)

`scripts/loopback-test.sh 30 --encoder x264 --width 1920 --height 1080 --bitrate-kbps 40000 --
--simulate-bandwidth <Mbit/s> --simulate-loss <pct>`; the viewer drops datagrams that exceed a
token bucket (150 ms deep) after the random-loss simulator. "Before" is b0c90ad with only the
simulator added; "after" is the adaptive-bitrate commit (adaptive on, FEC auto). 1800 frames sent.

| Link | Before: frames, fps, packet loss, unrecoverable, FEC | After: frames, fps, Mbit/s, packet loss, unrecoverable, FEC |
|---|---|---|
| 20 Mbit/s + 2 % random | 6, 0.2 fps, 69.5 %, 596, 59 % | 1612, 53.7 fps, 12.1 Mbit/s, 7.9 %, 67, 34 % |
| 8 Mbit/s + 2 % random | 2, 0.1 fps, 87.8 %, —, 59 % | 1389, 46.3 fps, 3.2 Mbit/s, 25.7 %, 220, 39 % |
| no limit + 2 % random | 1800, 60 fps, 40.0 Mbit/s, 2.0 %, 0, 17 % | 1801, 60 fps, 17.4 Mbit/s, 2.1 %, 0, 18 % |

Before, the encoder kept sending 40 Mbit/s plus up to 59 % parity into the bottleneck, so
almost nothing got through. After, the controller backs off to what fits and the stream stays
watchable; the mean capture→decoded latency stayed at 8–9 ms.

**Correction (2026-11-15): the last row is not a controller cut.** The same commit removed
x264's `nal-hrd=cbr` filler. Before, every frame was padded to the target, hence "40.0 Mbit/s".
Since then x264 sends only what the test pattern needs. Re-measured on the box (current
`main`, 30 s, x264 1920×1080, 40 Mbit/s setting; ~45–52 fps, since the VM was slow that day),
with every cut logged (`adaptive bitrate: cut … : <reason>` in the presenter log; the presenter
status shows `last cut: <reason>`):

| Run | Cuts | Budget | Sent (mean of 1 s windows) |
|---|---|---|---|
| adaptive on, 2 % random | 0 | 40.0 throughout | 15.4 Mbit/s (7–27 per window) |
| adaptive on, 0 % | 0 | 40.0 | 15.0 Mbit/s |
| adaptive off, 0 % | 0 | 40.0 (fixed) | 13.1 Mbit/s |
| adaptive off, 2 % random | 0 | 40.0 (fixed) | 15.0 Mbit/s |

So the ~15–17 Mbit/s is the content, not the rate control. 2 % random loss can't trigger a cut
on its own: the excess-loss rule needs 3 points above the background loss.

### 3.3 Adaptive bitrate, closed-loop simulation (unit test)

`cargo test -p beamcraft-proto sim_ -- --nocapture` runs the controller against an encoder
model (60 fps, sends min(target, content demand)), auto FEC parity, `LossSim`, a FIFO bottleneck
(150 ms buffer unless noted; overload shows as delay first, then as loss) and a `DelayMeter` as
the viewer has, with a 500 ms report. There is no QUIC, so the backlog trigger is not exercised.
Results with the controller as of 2026-11-16 (queue + loss only, no RTT rule), 40 Mbit/s
ceiling, 60–90 s; in brackets the 2026-11-15 controller (loss + RTT, drop-only token-bucket
bottleneck) where it differs:

| Link | Cuts (triggers) | Budget at end | Video delivered | Packets lost | Queue mean / p95 |
|---|---|---|---|---|---|
| no limit, 0 % / 2 % random | 0 | 40.0 | 36.8 / 33.8 Mbit/s | 0 / 2.0 % | 0 |
| no limit, 5 % random | 0 (old: 3, loss, until learned as background) | 40.0 | 30.5 Mbit/s | 5.1 % | 0 |
| content needs 15 Mbit/s, 2 % random | 0 | 24.4 | 15.2 Mbit/s | 2.0 % | 0 |
| content 12 Mbit/s with 24 Mbit/s bursts, 20 Mbit/s bottleneck | 6 queue (old 4) | 21.0 | 13.2 Mbit/s | 0 % (old 0.74 %) | 5.3 / 47.7 ms |
| 20 Mbit/s bottleneck, 0 % | 15 queue (old 11 heavy loss) | 17.7 | 16.7 Mbit/s (old 16.0) | 0 % (old 1.5 %) | 10.5 / 58.9 ms |
| 20 Mbit/s bottleneck, 2 % | 15 (2 loss, then queue) | 19.8 | 16.0 Mbit/s | 2.1 % | 11.2 / 56.9 ms |
| 8 Mbit/s bottleneck, 2 % | 22 (queue, 1 heavy loss) | 8.0 | 6.3 Mbit/s | 2.2 % (old 2.9 %) | 23.3 / 123 ms |
| 16 Mbit/s, 300 ms FIFO (`WIFI`) | 16 queue (RTT rule: 19) | 14.2 | 13.8 Mbit/s (RTT rule: 13.0) | 0 % | 18.1 / 70.5 ms (RTT rule: 25.9 / 129) |
| 16 Mbit/s, **50 ms** FIFO | 10 (1 queue, 9 heavy loss) | 18.4 | 11.7 Mbit/s | **4.9 %** | 10.1 / 34.4 ms |
| 16 Mbit/s, **20 ms** FIFO | 10 heavy loss | 13.6 | 11.3 Mbit/s | **5.5 %** | 1.5 / 4.4 ms |

Weak spots left (roadmap item 0): a bottleneck is still found by overrunning it every ~6 s (the
additive climb of 1.6 Mbit/s per second; item 0.4). With a **shallow** bottleneck buffer
(≤ 50 ms) the queue never reaches the 40 ms that marks loss as congestion, so loss counts as
random until it is heavy: each overrun loses ~23 % of a report's packets. Wi-Fi to the tablet
buffered ~290 ms, so it does not hit this; a shallow router or USB link would
(`sim_shallow_buffer_overruns_until_heavy_loss` pins it).

### 3.4 Real Wi-Fi: presenter in the dev container, Android tablet viewer (2026-11-15)

Presenter: this repo's dev container on the Fedora laptop (host network, Wi-Fi), test pattern,
x264 1920×1080@60, 40 Mbit/s setting, adaptive bitrate and FEC auto. Viewer: the MTK tablet
(TB336FU, `c2.mtk.avc.decoder` GPU output) over the same Wi-Fi, debug APK built here. 60 s per
run, before and after alternated so Wi-Fi conditions are comparable. "before" is 38e3290.

| Run | Unrecoverable frames | Keyframes sent | Viewer requests honoured / ignored | Cuts | 1 s windows with RTT > 150 ms | Video sent (mean) |
|---|---|---|---|---|---|---|
| before | 66 | 28 | 21 / – | 12 | 5 | 15.1 Mbit/s |
| fixed | 3 | 9 | 3 / 5 | 7 | 0 | 14.5 Mbit/s |
| before | 198 | 46 | 34 / – | 10 | 6 | 13.8 Mbit/s |
| fixed | 41 | 9 | 4 / 9 | 5 | 2 | 12.6 Mbit/s |

Earlier runs of the old build: 64, 93, 136, 102, 85 unrecoverable frames per 40–75 s.

How the cause was found: the tablet's `/proc/net/snmp` `RcvbufErrors` did not move (no kernel
drops), the presenter dropped nothing, but QUIC itself lost up to 1369 of 2921 packets in a
second (the presenter log now shows `quic lost N of M packets, cwnd`). The link to the tablet
carries ~20–25 Mbit/s; every loss episode was the presenter overrunning it: at connect
(starting at 40 Mbit/s, ~28 Mbit/s sent, RTT 250+ ms, 20–47 % loss), and later after the
budget had drifted to ~26 Mbit/s while only ~12 were sent, so the next busy frames jumped to
~24. Each episode then drew a keyframe request every ~100 ms and 4–8 IDRs a second. Fixed by
slow start, growing the budget only while it is used, and at most one forced keyframe per
2 × RTT. Limiting keyframes alone (measured separately) halved the IDRs but not the lost frames
(119 / 92 vs 102 / 85). The remaining episodes (fixed run 2) are bursts that really used a
~21–27 Mbit/s budget; the RTT rose 1–1.5 s before the loss each time (roadmap 0.2).

### 3.5 Real Wi-Fi: one-way queueing delay (2026-11-15, same setup as §3.4)

Same presenter and tablet as §3.4; this Wi-Fi link was weaker (tablet RSSI −75 dBm, 11n,
39 Mbit/s link speed) and carried ~15–17 Mbit/s. The APK reports the queueing delay in both
columns; "before" is 79eb65b's controller (ignores it), "queue" cuts on it. 60 s per run,
alternated. Queue statistics are over the viewer's 0.5 s reports (mean of the window means;
95th percentile of the window means).

| Run | Unrecoverable frames | Keyframes | Cuts | 1 s windows RTT > 150 ms | Video sent | Queue mean / p95 |
|---|---|---|---|---|---|---|
| before | 13 | 12 | 7 | 5 | 15.8 Mbit/s | 24 / 102 ms |
| queue (no max rule) | 10 | 10 | 8 | 0 | 14.7 Mbit/s | 20 / 65 ms |
| before | 1 | 9 | 9 | 0 | 15.6 Mbit/s | 21 / 85 ms |
| queue (no max rule) | 1 | 9 | 7 | 1 | 14.2 Mbit/s | 13 / 40 ms |
| before | 0 | 9 | 10 | 0 | 15.1 Mbit/s | 19 / 63 ms |
| queue | 0 | 9 | 10 | 0 | 14.1 Mbit/s | 13 / 36 ms |
| before | 35 | 13 | 8 | 4 | 15.3 Mbit/s | 32 / 129 ms |
| queue | 126 (121 in one outage) | 10 | 9 | 4 | 13.4 Mbit/s | 15 / 71 ms |

The queue controller lowers queueing delay every time (p95 −35 to −58 %) for ~7 % less video.
Lost frames are dominated by rare events that four minutes can't separate: the 121 came from a
Wi-Fi outage (no viewer report for 2.4 s, QUIC lost 2606 packets, queue 3 ms right after); the
rest of that run and the 10 in the first "queue" run are bursts after the budget had climbed
far above the link (27.5 Mbit/s budget while 10–15 were sent, then queue 4 → 128 ms in one
report). The first observation run (old controller) showed the shape of every episode: the
queue rises for 0.5–1.5 s (e.g. 42 → 104 → 211 → 292 ms) before any loss, while the RTT rule
fired only at 110–180 ms. "No max rule" missed 24 ms (max 82, +193 ms/s) one report before a
236 ms queue, which is why one frame waiting ≥ 50 ms now counts.

**Protocol 2 (2026-11-16, 5a181fa, APK rebuilt):** same setup, one 60 s run: connects, 121
`link_report`s with the queue object, 0 unrecoverable frames, 9 queue cuts, 14.7 Mbit/s sent,
queue mean 12 / p95 44 ms (within the spread of the four "queue" runs above). A protocol-1
desktop viewer (e298736) against this presenter is rejected with `version mismatch: the
presenter speaks protocol 2, the viewer 1; update the viewer …` in its connect steps and the
presenter's attempt list.

## 4. Real hardware

The owner tests on the target machines (decisions D11) and reports with **photos of the
screen**, not logs. So:

- Every state and error must be readable in the window: presenter status panel (phase,
  listen address, encoder, source, fingerprint, pairing code, connected viewer) and viewer
  connection status and stats overlay (`S`).
- Error messages say what to do ("No answer from 192.168.1.20:47800 (UDP). … On Fedora:
  sudo firewall-cmd --add-port=47800/udp"), not just what failed.
- Ask for exactly what you need ("press S and photograph the top-left corner").
- Don't ask him to install a toolchain on the iMac for routine debugging; build artifacts in CI.

## 5. CI and artifacts

- `ci.yml` (push to `main`, PRs): clippy `-D warnings`, `cargo test`, 5 s loopback with x264,
  then clippy/tests/5 s loopback again with feature `filmcraft` and `--encoder filmcraft`,
  the reusable audit workflow, and the reusable SonarCloud scan (`needs: audit`).
- `sonarcloud.yml` (called from CI; also from Build on `v*` tags and
  `workflow_dispatch`; can be dispatched on its own): if `SONAR_TOKEN` is
  missing (fork PRs), the job succeeds and skips the upload. Otherwise it
  writes a Clippy JSON report (`cargo clippy --message-format=json`) and
  unit-test coverage (`cargo llvm-cov --lcov`), then runs
  `SonarSource/sonarqube-scan-action` pinned to a full SHA. Report paths
  live in `sonar-project.properties` (`sonar.rust.clippyReport.reportPaths`,
  `sonar.rust.lcov.reportPaths`). The loopback script is not included in
  coverage, but the in-process loopback unit test is. `sonar.coverage.exclusions`
  lists the few whole files that can't run headless (egui/GL window code,
  portal/PipeWire capture, VAAPI/VideoToolbox device glue, firewalld D-Bus
  glue, the Android shell), each explained in a comment; keep it file-level
  and don't add logic files to it. The project uses the built-in *Sonar way*
  gate (new code coverage ≥ 80 %); custom gates need a paid plan. **Automatic Analysis must
  be off** under SonarCloud Administration › Analysis method for
  `windwerfer_beamcraft`; if it is still on, the upload fails and should
  not be worked around in the workflow.
- `codeql.yml` (push to `main`, pull requests, weekly Wednesday cron, and
  `workflow_dispatch`; also called from Build on `v*` tags and
  `workflow_dispatch`): GitHub CodeQL advanced setup for `rust` and
  `actions`, both with `build-mode: none` (no Linux media deps, no workspace
  compile). `github/codeql-action` init/analyze are pinned to a full SHA.
  Results upload to Security › Code scanning. **CodeQL default setup must
  be off** under Settings › Code security › Code scanning; if it is still
  on, the advanced workflow fails and should not be worked around.
  Branch-push Builds do not re-run this job, because the standalone
  workflow already ran on the same push.
- `audit.yml` (called from CI and Build; also `workflow_dispatch` and a weekly Monday cron):
  `cargo audit` against the RustSec advisory DB, and
  `cargo deny check advisories bans licenses sources` with `deny.toml`.
  `cargo audit` fails only on vulnerabilities; yanked and unmaintained crates are warnings.
  `cargo-deny` does the same for yanked crates, ignores unmaintained advisories (it has no
  warn-only level for them; `cargo audit` still prints those), fails on unsound crates and
  on any licence or source outside the allow-list, and warns on duplicate crate versions.
  Only crates.io is an allowed source. If an advisory hits a transitive dependency with no
  fix yet, add a justified `{ id = "RUSTSEC-…", reason = "…" }` ignore in `deny.toml` (and
  `--ignore` for `cargo audit` if needed) and report it; do not silently pass a vulnerability.
- `build.yml` (push to `main`, `v*` tags): the macOS, AppImage and Flatpak jobs all `needs:
  audit`, so a vulnerable or policy-violating lockfile never produces an artifact. Then:
  macOS arm64 DMG (`macos-15`), macOS x86_64 DMG (`macos-15-intel`, min 10.13), Flatpak,
  AppImage (Ubuntu 24.04); a `v*` tag creates a GitHub Release with all four. The Intel Mac
  job is the slowest (static FFmpeg build on older runners); that is normal.
- Artifacts are uploaded with `archive: false`, so each download is the bare `.dmg` /
  `.flatpak` / `.AppImage`.
- **Re-runs hide artifacts:** if a job is re-run, the run page shows the latest attempt, which
  only has the re-run job's artifact. The others are on `…/actions/runs/<id>/attempts/1`. When
  you hand out links, check every artifact exists and link the attempt that has it (`gh api
  repos/windwerfer/beamcraft/actions/runs/<id>/artifacts`).
- Do not change workflows as a side effect of a feature. A workflow change is its own commit.
