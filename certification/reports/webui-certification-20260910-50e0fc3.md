# TorrentNG WebUI Certification

- Generated: 2026-09-10T19:37:34Z
- Commit: 50e0fc3
- 15k first-visible threshold: 8000ms

| Gate | Result |
| --- | --- |
| webui production build | PASS |

## webui production build

```text
npm notice run torrentng-webui@0.1.0 build
npm notice run tsc && vite build
vite v8.2.2 building client environment for production...
transforming...
✓ 94 modules transformed.
rendering chunks...
computing gzip size...
../sidecar/static/index.html                             0.62 kB │ gzip:   0.41 kB
../sidecar/static/assets/index-CtGr9d3k.css             24.28 kB │ gzip:   4.68 kB
../sidecar/static/assets/UserAgentPanel-C2XiwE6r.js      5.51 kB │ gzip:   1.91 kB
../sidecar/static/assets/LogsPanel-CMJqDDZn.js           6.40 kB │ gzip:   2.01 kB
../sidecar/static/assets/RatioGroupsPanel--iTLp3di.js    7.54 kB │ gzip:   2.31 kB
../sidecar/static/assets/WorkflowsPanel-CAmQQ1tE.js     10.11 kB │ gzip:   2.86 kB
../sidecar/static/assets/RssRulesPanel-C265o7F7.js      10.25 kB │ gzip:   2.86 kB
../sidecar/static/assets/StoragePanel-CpDNOEkV.js       16.51 kB │ gzip:   4.26 kB
../sidecar/static/assets/EnginePanel-mDXN15pC.js        44.67 kB │ gzip:  10.87 kB
../sidecar/static/assets/index-DoaHm8ZH.js             437.55 kB │ gzip: 121.70 kB

✓ built in 458ms
```
| webui lint | PASS |

## webui lint

```text
npm notice run torrentng-webui@0.1.0 lint
npm notice run eslint src --ext ts,tsx --report-unused-disable-directives --max-warnings 0
```
| webui browser matrix | PASS |

## webui browser matrix

```text
npm notice run torrentng-webui@0.1.0 test:e2e
npm notice run playwright test --reporter=list
[WebServer] npm notice run torrentng-webui@0.1.0 preview
[WebServer] npm notice run vite preview --host 127.0.0.1 --port 4173
[WebServer] (node:782901) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
[WebServer] (Use `node --trace-warnings ...` to show where the warning was created)

Running 20 tests using 8 workers

(node:783023) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783016) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783015) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783014) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783046) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783029) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783017) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:783035) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
  -   1 [mobile] › tests/e2e/webui-scale-cert.spec.ts:195:1 › desktop handles 15k torrents without rendering every row
  ✓   3 [mobile] › tests/e2e/webui-visual-cert.spec.ts:147:1 › torrent workspace visual baseline (779ms)
  ✓   2 [mobile] › tests/e2e/webui-cert.spec.ts:189:1 › desktop renders torrent workspace and table rows (901ms)
  ✓   4 [desktop] › tests/e2e/webui-cert.spec.ts:189:1 › desktop renders torrent workspace and table rows (910ms)
  ✓   5 [desktop] › tests/e2e/webui-visual-cert.spec.ts:147:1 › torrent workspace visual baseline (915ms)
  ✓   6 [desktop] › tests/e2e/webui-scale-cert.spec.ts:195:1 › desktop handles 15k torrents without rendering every row (922ms)
  ✓   9 [mobile] › tests/e2e/webui-scale-cert.spec.ts:212:1 › core workspace controls expose accessible names (625ms)
  -  11 [mobile] › tests/e2e/webui-cert.spec.ts:198:1 › settings storage panel renders with mocked root
  -  10 [mobile] › tests/e2e/webui-visual-cert.spec.ts:154:1 › settings storage visual baseline
  ✓  14 [desktop] › tests/e2e/webui-scale-cert.spec.ts:212:1 › core workspace controls expose accessible names (558ms)
  ✓  15 [mobile] › tests/e2e/webui-scale-cert.spec.ts:252:1 › Ctrl+A selects the loaded page without opening add-torrent (483ms)
  ✓  12 [desktop] › tests/e2e/webui-cert.spec.ts:198:1 › settings storage panel renders with mocked root (796ms)
  ✓  13 [desktop] › tests/e2e/webui-visual-cert.spec.ts:154:1 › settings storage visual baseline (905ms)
  ✓  16 [mobile] › tests/e2e/webui-cert.spec.ts:207:1 › mobile viewport keeps primary actions reachable (758ms)
  -  18 [desktop] › tests/e2e/webui-cert.spec.ts:207:1 › mobile viewport keeps primary actions reachable
  ✓  17 [desktop] › tests/e2e/webui-scale-cert.spec.ts:252:1 › Ctrl+A selects the loaded page without opening add-torrent (397ms)
  ✓   8 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:133:1 › torrent workspace has no serious automated accessibility violations (1.9s)
  -  19 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:149:1 › settings library panel has no serious automated accessibility violations
  ✓   7 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:133:1 › torrent workspace has no serious automated accessibility violations (2.3s)
  ✓  20 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:149:1 › settings library panel has no serious automated accessibility violations (1.1s)

  5 skipped
  15 passed (4.3s)
```

Overall status: PASS
