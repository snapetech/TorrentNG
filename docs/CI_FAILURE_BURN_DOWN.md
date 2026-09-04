# TorrentNG CI Failure Burn-down

Status: **green on `main`** as of 2026-09-04. The final GitHub Actions CI
run was `33910318860` on commit `83b70ce`; all ten jobs passed. The dynamic
`Push on main` orchestration run `33910318425` also passed.

## Failures fixed

| Run / job | Actual failure | Fix | Evidence |
| --- | --- | --- | --- |
| `33906940869` / Backup and restore drill | The clean GitHub checkout did not contain the ignored recovery fixture. | `718e571` makes the drill create a valid tiny v1 fixture when the ignored local fixture is absent. | Final backup/restore job passed in `33910318860`. |
| `33906940869` / Backend fault-containment matrix | `yield_now()` was not a durable scheduling boundary on the hosted runner; the pause test raced the worker. | `718e571` changed the test to bounded deadline polling of durable SQLite state. | Full native suite and final fault job passed. |
| `33908414275` / native-quality | `tar -xOzf ... | grep -q` caused GNU tar to receive a broken pipe under `pipefail`. | `dc4ab9a` extracts the archive before checking its contents; `b748f59` applied the same fix to archive listing. | Certification bundle self-test and final native-quality job passed. |
| `33909060233` / native-quality | Cleanup became visible on disk before the terminal job row was durably removed, so the test asserted too early. | `8a85615` waits for both filesystem cleanup and the empty durable job projection. | The regression passed repeatedly locally and in final hosted CI. |
| Final interop harness path | `curl | grep -q` let `grep` exit early and surfaced curl's SIGPIPE as a false failure under `pipefail`. | `83b70ce` captures the metrics response and checks it with a here-string. | Current 28/28 Docker matrix and final CI are green. |

## Other CI hardening included

- `e226c03` waits for daemon readiness after the interop restart before
  probing facade endpoints.
- `70852de` makes the local security scan fail closed when required tooling is
  unavailable; explicitly allowing a blocked local tool produces a warning,
  not a false clean pass.
- `18b836c` makes the backup/recovery evidence portable and diagnosable in a
  clean checkout.

The cancelled intermediate CI runs were superseded by later pushes while the
same failure burn-down was in progress. They are not evidence of a remaining
failure. Dependabot alerts, CodeQL alerts, and secret-scanning alerts are all
currently zero.

## Current boundary

CI now covers native quality, both declared MSRV floors, fuzz smoke, sidecar,
WebUI, dependency security, backup/restore, API/SSE load, and deterministic
fault containment. It does not certify target-device storage, public-swarm
behavior, branch-protection enforcement, a 24-hour soak, production-corpus
allocator/fairness profiling, or optional extended-capacity claims.
