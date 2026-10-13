#!/usr/bin/env bash
# Terminal throughput, frame pacing and memory of muxus-rs, repeatably.
#
#   hack/perf.sh [--binary target/release/muxus] [--out DIR]
#
# Runs the app in a throwaway headless GNOME Shell (hack/shot.sh) with
# MUXUS_SCENE=perf, which in one plain bash tab (no rc files, scratch HOME):
#   1. idles for 3 s (baseline frames and memory),
#   2. runs `time seq 1 2000000` (about 15.9 MB of output),
#   3. runs `time cat ~/big.txt` (a 50 MB text file of log-like lines),
#   4. opens 9 more local shells and waits 6 s (memory with ten tabs),
# then quits. MUXUS_PERF_LOG records one line per second (frames painted,
# largest and 95th-percentile frame gap, output bytes handled, the longest
# output batch on the UI thread, resident memory) plus marks around each step.
# Next to it, a sampler reads the app process's CPU time (utime + stime from
# /proc) five times a second, so the summary also says how busy the process
# is in each step and once the output stops: right after `seq`, and with ten
# idle tabs (the last 3 s). The summary below is computed from both logs.
#
# The log ticks once a second, so a step's row is built from the ticks that
# overlap it: output from every tick that touches the step, frames and frame
# gaps from the ticks that lie mostly inside it. The tick that covers the
# opening of the nine tabs is left out of the `cat` row; that step reports the
# longest the UI thread was busy in one go (how late a 4 ms heartbeat woke),
# since a frame gap there is mostly idle time with nothing to paint. Now that
# `seq` and `cat` take about a second each, their frame numbers rest on one or
# two ticks.
#
# The headless shell renders with lavapipe (software Vulkan), so frame times
# are pessimistic compared with a GPU; compare runs with each other.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
binary="$root/target/debug/muxus"
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) binary="$(realpath -m "$2")"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[[ -x "$binary" ]] || { echo "no binary at $binary (cargo build -p muxus-app first)" >&2; exit 2; }
if [[ -z "$out" ]]; then out="$(mktemp -d "${TMPDIR:-/tmp}/muxus-perf-XXXXXX")"; fi
mkdir -p "$out"
out="$(realpath "$out")"

home="$out/home"
rm -rf "$home"
mkdir -p "$home"
# 50 MB of log-like text: ~100-character lines with a timestamp, level and message.
python3 -I - "$home/big.txt" <<'EOF'
import sys
levels = ["INFO", "WARN", "DEBUG", "ERROR"]
with open(sys.argv[1], "w") as f:
    written, i = 0, 0
    while written < 50 * 1024 * 1024:
        line = (f"2026-03-12T09:{i // 3600 % 60:02d}:{i % 60:02d}.{i % 1000:03d}Z {levels[i % 4]:5} "
                f"edge[{1000 + i % 97}]: request id={i:08x} path=/api/v1/items/{i % 9973} status=200 bytes={i % 65536}\n")
        f.write(line)
        written += len(line)
        i += 1
EOF

log="$out/perf.log"
cpu="$out/cpu.log"
rm -f "$log" "$cpu"
# The CPU sampler: finds the app by its executable and scratch HOME (never
# another muxus), then logs "seconds cpu-seconds last-mark" until it exits.
python3 -I - "$binary" "$home" "$log" "$cpu" <<'EOF' &
import os, sys, time
binary, home, log, out = sys.argv[1:5]
binary = os.path.realpath(binary)
want = f"HOME={home}".encode()
tick = os.sysconf("SC_CLK_TCK")

def find():
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            if os.path.realpath(f"/proc/{pid}/exe") != binary:
                continue
            with open(f"/proc/{pid}/environ", "rb") as f:
                if want in f.read().split(b"\0"):
                    return pid
        except OSError:
            pass
    return None

deadline = time.monotonic() + 120
pid = None
while pid is None and time.monotonic() < deadline:
    pid = find()
    if pid is None:
        time.sleep(0.1)
if pid is None:
    sys.exit(0)
with open(out, "w") as f:
    while True:
        try:
            with open(f"/proc/{pid}/stat") as stat:
                fields = stat.read().rsplit(")", 1)[1].split()
        except OSError:
            break
        mark = ""
        try:
            with open(log) as perf:
                for line in perf:
                    parts = line.split()
                    if len(parts) > 2 and parts[1] == "mark":
                        mark = parts[2]
        except OSError:
            pass
        f.write(f"{time.monotonic():.3f} {(int(fields[11]) + int(fields[12])) / tick:.2f} {mark}\n")
        f.flush()
        time.sleep(0.2)
EOF
sampler=$!
# The app quits when the scene is done; shot.sh then reports an early exit,
# which is expected here.
"$here/shot.sh" --name perf --wait 600 --home "$home" \
  --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=perf --env MUXUS_THEME=dark \
  --env MUXUS_PERF_LOG="$log" \
  "$out/last.png" -- "$binary" >/dev/null 2>&1 || true
wait "$sampler" 2>/dev/null || true

[[ -s "$log" ]] || { echo "no perf log; see $out/last.png.log" >&2; exit 1; }

python3 -I - "$log" "$cpu" <<'EOF'
import os, re, sys

lines = open(sys.argv[1]).read().splitlines()
marks, ticks, notes = {}, [], []
for line in lines:
    t, _, rest = line.partition(" ")
    t = float(t)
    if rest.startswith("mark "):
        label, _, fields = rest[5:].partition(" ")
        marks[label] = (t, dict(kv.split("=") for kv in fields.split()))
    elif rest.startswith("tick "):
        ticks.append((t, dict(kv.split("=") for kv in rest[5:].split())))
    elif rest.startswith("note "):
        notes.append(rest[5:])

def phase(name):
    start, end = marks[f"{name}-start"][0], marks[f"{name}-end"][0]
    # A tick covers the second before it. Output is counted from every tick that touches the
    # step; frames and gaps only from the ticks that mostly lie inside it, so the step before
    # or after (an idle prompt, tabs opening) does not leak in.
    def overlap(t):
        return min(t, end) - max(t - 1.0, start)
    touching = [f for t, f in ticks if overlap(t) > 0]
    # Opening the nine tabs right after `cat` stalls frames for a few hundred milliseconds;
    # the tick that covers it is not the flood's.
    opening = marks["one-tab"][0] if "one-tab" in marks else None
    flood = [(t, f) for t, f in ticks if overlap(t) > 0 and not (opening and t - 1.0 < opening < t)]
    used = [(t, f) for t, f in flood if overlap(t) > 0.5] or flood
    inside = [f for _, f in used] or touching
    # Frames per second while the step runs: the average tick (a frame is only painted when
    # something changed, so idle seconds count low).
    rate = sum(int(f["frames"]) for f in inside) / len(inside) if inside else 0.0
    seconds = end - start
    gaps = [float(f["max_gap_ms"]) for f in inside if int(f["frames"]) > 1]
    p95 = [float(f["p95_gap_ms"]) for f in inside if int(f["frames"]) > 1]
    batch = max((float(f["max_batch_ms"]) for f in inside), default=0.0)
    mb = sum(int(f["bytes"]) for f in touching) / 1e6
    rss = max((int(f["rss_kib"]) for f in inside), default=0) / 1024
    print(f"{name:5}  wall {seconds:6.2f} s  output {mb:6.1f} MB ({mb / seconds if seconds else 0:5.1f} MB/s)  "
          f"frames {rate:5.1f}/s  worst gap {max(gaps, default=0):6.1f} ms  "
          f"worst p95 gap {max(p95, default=0):6.1f} ms  longest batch {batch:6.1f} ms  peak RSS {rss:6.1f} MiB")

for name in ("idle", "seq", "cat"):
    if f"{name}-start" in marks and f"{name}-end" in marks:
        phase(name)
if "one-tab" in marks and "ten-tabs" in marks:
    # Opening nine tabs: how long the UI thread was busy in one go (frame gaps would also
    # count the idle time when nothing needed painting).
    start, end = marks["one-tab"][0], marks["ten-tabs"][0]
    opening = [f for t, f in ticks if t > start and t - 1.0 < end]
    block = max((float(f.get("max_block_ms", 0)) for f in opening), default=0.0)
    print(f"open 9 tabs  longest UI block {block:6.1f} ms")
for label in ("one-tab", "ten-tabs"):
    if label in marks:
        print(f"RSS with {label:8}: {int(marks[label][1]['rss_kib']) / 1024:7.1f} MiB")
samples = []
if os.path.exists(sys.argv[2]):
    for line in open(sys.argv[2]).read().splitlines():
        parts = line.split()
        samples.append((float(parts[0]), float(parts[1]), parts[2] if len(parts) > 2 else ""))

def cpu_share(mark, skip=0.0):
    inside = [s for s in samples if s[2] == mark]
    if inside:
        inside = [s for s in inside if s[0] >= inside[0][0] + skip]
    if len(inside) < 2 or inside[-1][0] <= inside[0][0]:
        return None
    return 100.0 * (inside[-1][1] - inside[0][1]) / (inside[-1][0] - inside[0][0])

shares = [(label, cpu_share(mark, skip)) for label, mark, skip in (
    ("idle", "idle-start", 0.0),
    ("seq", "seq-start", 0.0),
    ("after seq", "seq-end", 0.0),
    ("cat", "cat-start", 0.0),
    ("ten idle tabs", "one-tab", 3.0),
)]
if any(share is not None for _, share in shares):
    print("CPU of the app process (% of one core): " + ", ".join(
        f"{label} {share:.1f}%" for label, share in shares if share is not None))
if notes:
    print("shell timing (bash `time`):")
    for note in notes:
        if re.match(r"^(real|user|sys)\b", note.strip()):
            print("   ", note.strip())
EOF
echo "log: $log"
