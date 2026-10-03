#!/bin/sh
# Print one validated torrentngd process sample as tab-separated fields:
# pid, executable, RSS KiB, fd count, thread count.
# SOAK_PROC_ROOT is only intended for the self-test fixture.
set -eu

proc_root="${SOAK_PROC_ROOT:-/proc}"
if [ "${1:-}" = --host-process ]; then
  pid="${2:-}"
  exe="${3:-}"
  case "$pid" in ''|*[!0-9]*) echo "invalid host process PID" >&2; exit 2 ;; esac
  case "$exe" in */torrentngd) ;; *) echo "invalid daemon executable from container process table" >&2; exit 2 ;; esac
  [ "$pid" != 1 ] || { echo "refusing to sample host PID 1" >&2; exit 2; }
  [ "$(cat "$proc_root/$pid/comm" 2>/dev/null || true)" = torrentngd ] || {
    echo "container process table PID is not torrentngd" >&2
    exit 2
  }
  status="$proc_root/$pid/status"
  [ -r "$status" ] || { echo "daemon status unavailable" >&2; exit 2; }
  rss_kib="$(awk '$1 == "VmRSS:" {print $2; found=1} END {if (!found) exit 1}' "$status")" || {
    echo "daemon RSS unavailable" >&2
    exit 2
  }
  threads="$(awk '$1 == "Threads:" {print $2; found=1} END {if (!found) exit 1}' "$status")" || {
    echo "daemon thread count unavailable" >&2
    exit 2
  }
  fds=0
  for fd in "$proc_root/$pid"/fd/*; do
    [ -L "$fd" ] && fds=$((fds + 1))
  done
  case "$rss_kib:$fds:$threads" in
    *[!0-9:]*|:*|*:) echo "invalid daemon resource sample" >&2; exit 2 ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\n' "$pid" "$exe" "$rss_kib" "$fds" "$threads"
  exit 0
fi

mode="${1:-sample}"
matches=""
count=0
for comm_path in "$proc_root"/[0-9]*/comm; do
  [ -r "$comm_path" ] || continue
  pid="${comm_path%/comm}"
  pid="${pid##*/}"
  [ "$pid" != 1 ] || continue
  IFS= read -r comm < "$comm_path" || true
  [ "$comm" = torrentngd ] || continue
  [ "$mode" = sample ] || { echo "usage: $0 [--host-process PID /path/to/torrentngd]" >&2; exit 2; }
  exe="$(readlink "$proc_root/$pid/exe" 2>/dev/null || true)"
  case "$exe" in
    */torrentngd) ;;
    *) continue ;;
  esac
  matches="$matches $pid:$exe"
  count=$((count + 1))
done

[ "$count" -eq 1 ] || {
  echo "expected exactly one torrentngd process, found $count" >&2
  exit 2
}

set -- $matches
pid_exe="$1"
pid="${pid_exe%%:*}"
exe="${pid_exe#*:}"
status="$proc_root/$pid/status"
[ -r "$status" ] || { echo "daemon status unavailable" >&2; exit 2; }
rss_kib="$(awk '$1 == "VmRSS:" {print $2; found=1} END {if (!found) exit 1}' "$status")" || {
  echo "daemon RSS unavailable" >&2
  exit 2
}
threads="$(awk '$1 == "Threads:" {print $2; found=1} END {if (!found) exit 1}' "$status")" || {
  echo "daemon thread count unavailable" >&2
  exit 2
}
fds=0
for fd in "$proc_root/$pid"/fd/*; do
  [ -L "$fd" ] && fds=$((fds + 1))
done
case "$rss_kib:$fds:$threads" in
  *[!0-9:]*|:*|*:) echo "invalid daemon resource sample" >&2; exit 2 ;;
esac
printf '%s\t%s\t%s\t%s\t%s\n' "$pid" "$exe" "$rss_kib" "$fds" "$threads"
