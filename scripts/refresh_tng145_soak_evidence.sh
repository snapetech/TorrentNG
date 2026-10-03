#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_REPORT="${1:?usage: refresh_tng145_soak_evidence.sh SOURCE_REPORT FINAL_REPORT}"
FINAL_REPORT="${2:?usage: refresh_tng145_soak_evidence.sh SOURCE_REPORT FINAL_REPORT}"

python3 - "$ROOT" "$SOURCE_REPORT" "$FINAL_REPORT" <<'PY'
from pathlib import Path
import os
import re
import sys
import tempfile

root, source_report, final_report = map(Path, sys.argv[1:])
if not source_report.is_file() or not final_report.is_file():
    raise SystemExit("soak source and finalization reports must both exist")
source = source_report.read_text()
final = final_report.read_text()
match = re.search(r"^Overall status: (PASS|FAIL)$", final, re.M)
if not match:
    raise SystemExit("finalization report has no recognized overall status")
status = match.group(1)
counts = re.search(r"^\| sample count \| PASS \| (\d+) >= (\d+) \|$", final, re.M)
sample_detail = f"{counts.group(1)} samples" if counts else "sample count below qualification minimum"
sample_rows = re.findall(r"^\| (20\d\d-\d\d-\d\dT[^|]+) \|", source, re.M)
start, end = (sample_rows[0].strip(), sample_rows[-1].strip()) if sample_rows else ("unknown", "unknown")
final_link = "[soak-final-tng145-current-20261003.md](../certification/reports/soak-final-tng145-current-20261003.md)"

if status == "PASS":
    outcome = f"The current-tree 24-hour idle-daemon qualification completed PASS ({sample_detail}; {start} through {end}) under the recorded daemon resource and service-health thresholds. This remains idle-daemon evidence, not loaded-swarm or public-network evidence."
    ledger_status = "Sampler fixed; fresh 24-hour idle-daemon soak PASS"
    release_scope = f"{final_link}; PASS ({sample_detail}; {start} through {end}). Empty state, DHT disabled, no torrent workload; qualifies idle-daemon ceilings only, not loaded-swarm or public-network behavior."
    rectification = "The current-tree 24-hour idle-daemon soak completed PASS; see the finalization report for sample-count and daemon RSS/FD/thread ceilings. Historical resource values remain invalid, and a public-torrent loaded soak is not covered."
else:
    outcome = f"The current-tree 24-hour idle-daemon qualification did not pass ({sample_detail}; {start} through {end}); TNG-145 remains open. See the finalization report for failed gates."
    ledger_status = "Sampler fixed; fresh 24-hour idle-daemon soak did not pass"
    release_scope = f"{final_link}; FAIL ({sample_detail}; {start} through {end}). TNG-145 remains open; see failed finalization gates."
    rectification = "The current-tree 24-hour idle-daemon qualification did not pass; TNG-145 remains open. See the finalization report for failed gates. Historical resource values remain invalid, and this run does not cover a public-torrent loaded soak."

def replace_once(text, pattern, replacement, name):
    result, n = re.subn(pattern, replacement, text, count=1, flags=re.M | re.S)
    if n != 1:
        raise SystemExit(f"expected one pending {name} section, found {n}")
    return result

updates = {}
p = root / "docs/BACKEND_AUDIT_BURN_DOWN.md"
t = p.read_text()
t = replace_once(t, r"^Status: \*\*audit refresh 2026-10-02:[^\n]*\*\*$",
                 f"Status: **audit refresh 2026-10-03: TNG-145 current-tree soak {status}; TNG-146 physical SSD evidence recorded; TNG-147 claims and navigation reconciled.**", "backend audit status")
t = replace_once(t, r"release rollup requires.*?24-hour daemon soak is active and remains unqualified until completion\.",
                 "release rollup requires at least one physical-device-qualified target for a hardware PASS. Focused tests cover wrapper-only/ambiguous process sampling and target classification. Fresh physical-device evidence passes on one local SSD. " + outcome, "backend audit summary")
t = replace_once(t, r"The run uses a disposable empty state, disabled DHT, and no torrent workload\. Its final result is not yet known\.",
                 "The run uses a disposable empty state, disabled DHT, and no torrent workload. " + outcome, "soak run summary")
t = replace_once(t, r"The\n24-hour soak is explicitly deferred to a later test window\.",
                 "The\ncurrent-tree idle-daemon soak outcome is recorded in " + final_link + ".", "executive soak scope")
t = replace_once(t, r"\*\*Status: Sampler fixed; fresh 24-hour idle-daemon soak in progress\*\*",
                 f"**Status: {ledger_status}**", "TNG-145 ledger status")
t = replace_once(t, r"resource checks\. Each report records container and image IDs and the sampled\ndaemon PID/executable\. The current-tree run is\n.*?; it began with one clean sample and will qualify idle daemon ceilings only if all 24 hours complete cleanly\. The historical public soak remains continuity evidence only\.",
                 "resource checks. Each report records container and image IDs and the sampled daemon PID/executable. The current-tree run is documented in the source report; " + outcome + " The historical public soak remains continuity evidence only.", "TNG-145 resolution")
marker = "| 2026-10-02 | Fixed daemon resource sampling to identify"
if marker not in t:
    raise SystemExit("burn-down log insertion point missing")
log_row = (f"| 2026-10-03 | Finalized TNG-145 current-tree idle-daemon soak: {status}. | "
           f"{final_link}; {sample_detail}; source samples {start} through {end}. | " +
           ("TNG-145 idle-daemon resource qualification closed for its recorded scope; no loaded-swarm claim." if status == "PASS" else "TNG-145 remains open pending a passing soak.") + " |")
if log_row not in t:
    i = t.index("\n", t.index(marker))
    t = t[:i + 1] + log_row + "\n" + t[i + 1:]
updates[p] = t

p = root / "docs/RELEASE_EVIDENCE.md"
t = p.read_text()
t = replace_once(t, r"\| Current-tree 24-hour daemon soak \| IN_PROGRESS \|[^\n]*",
                 f"| Current-tree 24-hour daemon soak | {status} | [soak-24h-tng145-current-20261002.md](../certification/reports/soak-24h-tng145-current-20261002.md) and {release_scope} |", "release soak row")
t = replace_once(t, r"The current qualification also defers the 24-hour soak;\nrun the non-soak gates with .*?A later soak must target the then-current artifact before making a\nstability claim\.",
                 f"The current-tree soak result is recorded above and in {final_link}. Its scope is limited to an idle daemon; it does not establish loaded-swarm stability.", "release scope note")
updates[p] = t

p = root / "docs/PROJECT_GAP_AUDIT.md"
t = p.read_text()
t = replace_once(t, r"A current-tree 24-hour idle-daemon soak began on 2026-10-02; its samples pass so far, but the full duration is still in progress\. No public torrent is loaded and DHT is disabled, so it is not loaded-swarm or public-network evidence\.",
                 outcome, "project gap soak note")
updates[p] = t

p = root / "docs/PERFORMANCE_SCALE_CLAIMS_AUDIT.md"
t = p.read_text()
t = replace_once(t, r"1\. \*\*Soak telemetry — implemented; fresh soak in progress\.\*\*",
                 f"1. **Soak telemetry — implemented; fresh soak {status}.**", "performance soak status")
t = replace_once(t, r"IDs\. A current-tree 24-hour idle-daemon soak is running; the historical\n   resource values remain invalid, and a public-torrent loaded soak is not\n   covered by this run\.",
                 "IDs. " + rectification, "performance soak detail")
updates[p] = t

for path, text in updates.items():
    fd, temp = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        mode = path.stat().st_mode
        with os.fdopen(fd, "w") as handle:
            handle.write(text)
        os.chmod(temp, mode)
        os.replace(temp, path)
    except BaseException:
        try:
            os.unlink(temp)
        except FileNotFoundError:
            pass
        raise
print(f"refreshed TNG-145–147 audit and release evidence: {status}")
PY
