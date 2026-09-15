# TorrentNG CI Failure Burn-down

Status: **green on `main`** as of 2026-09-15. The latest completed GitHub
Actions CI run was `35016899959` on commit `56d77e5`; all ten jobs passed.
CodeQL run `35016899910` on the same commit also passed across actions, Rust,
JavaScript/TypeScript, and Python analysis.

## Failures fixed

| Run / job | Actual failure | Fix | Evidence |
| --- | --- | --- | --- |
| `33906940869` / Backup and restore drill | The clean GitHub checkout did not contain the ignored recovery fixture. | `718e571` makes the drill create a valid tiny v1 fixture when the ignored local fixture is absent. | Final backup/restore job passed in `33916500668`. |
| `33906940869` / Backend fault-containment matrix | `yield_now()` was not a durable scheduling boundary on the hosted runner; the pause test raced the worker. | `718e571` changed the test to bounded deadline polling of durable SQLite state. | Full TorrentNG-client suite and final fault job passed in `33916500668`. |
| `33908414275` / `native-quality` | `tar -xOzf ... | grep -q` caused GNU tar to receive a broken pipe under `pipefail`. | `dc4ab9a` extracts the archive before checking its contents; `b748f59` applied the same fix to archive listing. | Certification bundle self-test and final `native-quality` job passed in `33916500668`. |
| `33909060233` / `native-quality` | Cleanup became visible on disk before the terminal job row was durably removed, so the test asserted too early. | `8a85615` waits for both filesystem cleanup and the empty durable job projection. | The regression passed repeatedly locally and in final hosted CI `33916500668`. |
| Final interop harness path | `curl | grep -q` let `grep` exit early and surfaced curl's SIGPIPE as a false failure under `pipefail`. | `83b70ce` captures the metrics response and checks it with a here-string. | Current 28/28 Docker matrix and final CI `33916500668` are green. |
| `35014022485` / `native-quality` | A pure-v2 magnet recheck updated runtime `amount_left` but left the tracker detail row at its pre-recheck `left_bytes` value. | `7f66041` flushes runtime and tracker state after the recheck before acknowledging the command, and retains peer availability updates across rechecks. | The regression passed repeatedly locally; all ten jobs passed in hosted CI `35016899959`. |
| `35016119210` / `webui` | The accessibility scan sometimes sampled a properties dialog during its 140 ms entrance animation, producing a transient contrast violation. | `56d77e5` waits for the dialog opacity to settle at `1` before running axe. | Ten repeated dialog scans, the full local browser suite, and hosted CI `35016899959` passed. |

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

CI now covers TorrentNG-client quality, both declared MSRV floors, fuzz smoke,
the compatible-client service, WebUI, dependency security, backup/restore, API/SSE load, and deterministic
fault containment. The latest CodeQL orchestration also passed actions, Rust,
JavaScript/TypeScript, and Python analysis. It does not certify target-device
storage, public-swarm behavior, branch-protection enforcement, a 24-hour soak, production-corpus
allocator/fairness profiling, or optional extended-capacity claims.
