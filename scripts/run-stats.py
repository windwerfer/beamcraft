#!/usr/bin/env python3
"""Summarise presenter logs, one row per run (scripts/android-wifi-run.sh, or any presenter log).

usage: scripts/run-stats.py <presenter-log>...

From the per-second `presenter:` lines that carry a viewer report: mean video sent, keyframes,
queue mean / p95 (of the per-second viewer queue), QUIC packets lost, and the viewer's
unrecoverable frames (a running total); from `adaptive bitrate: cut` lines: cuts per trigger.
"""
import re
import statistics as st
import sys

STATS = re.compile(r" presenter: [\d.]+ fps")


def summarise(path):
    lines = open(path, errors="replace").read().split("\n")
    rows = [l for l in lines if STATS.search(l) and "viewer queue" in l]
    num = lambda pat, l: re.search(pat, l)
    video = [float(num(r"fps, ([\d.]+) Mbit/s", l).group(1)) for l in rows]
    keyframes = sum(int(num(r"Mbit/s, (\d+) keyframes", l).group(1)) for l in rows)
    queue = sorted(float(num(r"viewer queue (\d+) ms", l).group(1)) for l in rows)
    quic = [tuple(map(int, m.groups())) for l in rows if (m := num(r"quic lost (\d+) of (\d+)", l))]
    unrec = [int(m.group(1)) for l in rows if (m := num(r"unrecoverable (\d+)", l))]
    triggers = {}
    for l in lines:
        if "adaptive bitrate: cut" not in l:
            continue
        reason = l.split("Mbit/s: ", 1)[-1]
        kind = next((k for k in ("heavy", "loss", "queue") if reason.startswith(k)), "backlog" if "send queue" in reason else "other")
        triggers[kind] = triggers.get(kind, 0) + 1
    lost, sent = sum(a for a, _ in quic), sum(b for _, b in quic)
    p95 = queue[max(int(len(queue) * 0.95) - 1, 0)] if queue else 0
    cuts = sum(triggers.values())
    return (
        f"{path}: {len(rows)} s, video {st.mean(video) if video else 0:.1f} Mbit/s, "
        f"keyframes {keyframes}, cuts {cuts} {triggers or ''}, "
        f"queue mean {st.mean(queue) if queue else 0:.0f} p95 {p95:.0f} ms, "
        f"quic lost {lost}/{sent} ({100 * lost / max(sent, 1):.1f} %), "
        f"unrecoverable {unrec[-1] if unrec else '?'}"
    )


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    for p in sys.argv[1:]:
        print(summarise(p))
