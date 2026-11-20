#!/bin/zsh
# Measures the running tether's efficiency: CPU time per megabyte actually carried.
#
# Usage:  scripts/bench.sh [label] [bytes]
#   label  free text printed in the header            (default: "bench")
#   bytes  size of the test download that generates load (default: 30000000)
#
# Requirements: a connected, online tether (`usb-android-tethering status` says "online").
# No root needed.
#
# Methodology: CPU time comes from `ps`; bytes carried come from the tether's own counters
# (`status --json`, rx+tx). Both are sampled right after the status file refreshes, so they
# cover the same interval, and background traffic (other apps, VPN) is counted on both
# sides instead of skewing the ratio. Throughput depends on the mobile network, so compare
# "CPU per MB carried" between builds rather than raw speed.
set -euo pipefail

label=${1:-bench}
bytes=${2:-30000000}
status_file=/var/run/usb-android-tethering.json

pid=$(pgrep -fl usb-android-tethering | grep -v -- '__guard' | awk 'NR==1{print $1}') || pid=""
if [[ -z "$pid" || ! -r "$status_file" ]]; then
  print -u2 "no online usb-android-tethering instance found"
  exit 1
fi

cpu_seconds() {
  ps -o time= -p "$pid" | awk -F: '{ s = $NF; if (NF > 1) s += 60 * $(NF-1); if (NF > 2) s += 3600 * $(NF-2); print s }'
}
carried_bytes() {
  python3 -c 'import json,sys; t=json.load(open(sys.argv[1]))["traffic"]; print(t["rx_bytes"]+t["tx_bytes"])' "$status_file"
}
# Wait for the next status refresh (at most every 5 s), then sample CPU and counters together.
sample() {
  local m0; m0=$(stat -f %m "$status_file")
  for _ in {1..70}; do [[ $(stat -f %m "$status_file") != "$m0" ]] && break; sleep 0.1; done
  print "$(cpu_seconds) $(carried_bytes) $(date +%s.%N)"
}

print "== $label (pid $pid)"
top -l 2 -s 10 -pid "$pid" -stats pid,cpu,idlew | tail -1 |
  awk '{ printf "background (10 s): cpu %s%%  idle wakeups %s\n", $2, $3 }'

read c0 b0 t0 <<< "$(sample)"
if ! curl -sS -m 180 -o /dev/null -w 'download:          %{size_download} B at %{speed_download} B/s\n' \
  "https://speed.cloudflare.com/__down?bytes=${bytes}"; then
  print -u2 "test download failed; check connectivity and re-run"
  exit 1
fi
read c1 b1 t1 <<< "$(sample)"

awk -v c="$((c1 - c0))" -v b="$((b1 - b0))" -v w="$((t1 - t0))" 'BEGIN {
  printf "carried:           %.1f MB in %.1f s (%.1f MB/s)\n", b / 1e6, w, b / 1e6 / w
  printf "cpu:               %.2f s = %.1f%% of one core\n", c, 100 * c / w
  printf "cpu per MB:        %.1f ms\n", (b > 0) ? 1000 * c / (b / 1e6) : 0
}'
ps -o rss= -p "$pid" | awk '{ printf "memory (RSS):      %.1f MB\n", $1 / 1024 }'
