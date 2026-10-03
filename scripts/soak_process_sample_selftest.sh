#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

make_process() {
  local pid="$1" comm="$2" exe="$3"
  mkdir -p "$tmp/proc/$pid/fd"
  printf '%s\n' "$comm" > "$tmp/proc/$pid/comm"
  ln -s "$exe" "$tmp/proc/$pid/exe"
  printf 'Name:\t%s\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t4096 kB\nThreads:\t7\n' "$comm" > "$tmp/proc/$pid/status"
  ln -s /dev/null "$tmp/proc/$pid/fd/0"
  ln -s /dev/null "$tmp/proc/$pid/fd/1"
}

make_process 1 tini /usr/bin/tini
make_process 22 torrentngd /usr/local/bin/torrentngd
sample="$(SOAK_PROC_ROOT="$tmp/proc" "$ROOT/scripts/soak_process_sample.sh")"
[[ "$sample" == $'22\t/usr/local/bin/torrentngd\t4096\t2\t7' ]]
host_sample="$(SOAK_PROC_ROOT="$tmp/proc" "$ROOT/scripts/soak_process_sample.sh" --host-process 22 /usr/local/bin/torrentngd)"
[[ "$host_sample" == "$sample" ]]
if SOAK_PROC_ROOT="$tmp/proc" "$ROOT/scripts/soak_process_sample.sh" --host-process 22 /opt/wrapper >/dev/null 2>&1; then
  echo "host sampler accepted a non-daemon executable" >&2
  exit 1
fi

rm -rf "$tmp/proc/22"
if SOAK_PROC_ROOT="$tmp/proc" "$ROOT/scripts/soak_process_sample.sh" >/dev/null 2>&1; then
  echo "sampler accepted wrapper-only process" >&2
  exit 1
fi

make_process 22 torrentngd /usr/local/bin/torrentngd
make_process 23 torrentngd /opt/torrentngd
if SOAK_PROC_ROOT="$tmp/proc" "$ROOT/scripts/soak_process_sample.sh" >/dev/null 2>&1; then
  echo "sampler accepted ambiguous daemon processes" >&2
  exit 1
fi

echo "soak process sampler self-test: PASS"
