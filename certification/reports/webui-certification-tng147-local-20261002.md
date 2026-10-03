# TorrentNG WebUI Certification

- Generated: 2026-10-02T21:25:40Z
- Commit: db4e333c
- Worktree state: dirty
- 15k first-visible threshold: 8000ms

| Gate | Result |
| --- | --- |
| webui production build | PASS |

## webui production build

```text
npm notice run torrentng-webui@0.1.0 build
npm notice run tsc && vite build
vite v8.3.0 building client environment for production...
transforming...
<script src="./runtime-config.js"> in "/index.html" can't be bundled without type="module" attribute
✓ 99 modules transformed.
rendering chunks...
computing gzip size...
../sidecar/static/index.html                             0.62 kB │ gzip:   0.41 kB
../sidecar/static/assets/index-Cc49jgGB.css             30.96 kB │ gzip:   5.70 kB
../sidecar/static/assets/AuthPanel-Bq8M5RxX.js           4.05 kB │ gzip:   1.55 kB
../sidecar/static/assets/UserAgentPanel-DSlwpSsn.js      5.81 kB │ gzip:   2.01 kB
../sidecar/static/assets/LogsPanel-Hcb-iLwF.js           6.65 kB │ gzip:   2.11 kB
../sidecar/static/assets/RatioGroupsPanel-VqTKBNqc.js    7.90 kB │ gzip:   2.44 kB
../sidecar/static/assets/RssRulesPanel-C73dSprh.js      10.74 kB │ gzip:   3.04 kB
../sidecar/static/assets/WorkflowsPanel-CWcpd_cW.js     10.76 kB │ gzip:   3.06 kB
../sidecar/static/assets/StoragePanel-eCbc6u_g.js       16.32 kB │ gzip:   4.20 kB
../sidecar/static/assets/CrashSafetyPanel-Bg3Jkicn.js   29.34 kB │ gzip:   9.14 kB
../sidecar/static/assets/EnginePanel-CLNgkzuf.js        45.14 kB │ gzip:  11.00 kB
../sidecar/static/assets/index-hZ8yeSH6.js             501.70 kB │ gzip: 139.63 kB

✓ built in 528ms
[plugin builtin:vite-reporter]
(!) Some chunks are larger than 500 kB after minification. Consider:
- Using dynamic import() to code-split the application
- Use build.rolldownOptions.output.codeSplitting to improve chunking: https://rolldown.rs/reference/OutputOptions.codeSplitting
- Adjust chunk size limit for this warning via build.chunkSizeWarningLimit.
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
[WebServer] (node:86183) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
[WebServer] (Use `node --trace-warnings ...` to show where the warning was created)

Running 38 tests using 8 workers

(node:86258) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86272) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86274) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86260) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86266) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86259) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86290) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
(node:86282) Warning: The 'NO_COLOR' env is ignored due to the 'FORCE_COLOR' env being set.
(Use `node --trace-warnings ...` to show where the warning was created)
  -   1 [mobile] › tests/e2e/webui-scale-cert.spec.ts:198:1 › desktop handles 15k torrents without rendering every row
  ✓   6 [mobile] › tests/e2e/webui-visual-cert.spec.ts:164:1 › torrent workspace visual baseline (587ms)
  ✓   4 [mobile] › tests/e2e/webui-cert.spec.ts:203:1 › desktop renders torrent workspace and table rows (718ms)
  ✓   3 [desktop] › tests/e2e/webui-cert.spec.ts:203:1 › desktop renders torrent workspace and table rows (748ms)
  ✓   2 [desktop] › tests/e2e/webui-visual-cert.spec.ts:164:1 › torrent workspace visual baseline (757ms)
  ✓   5 [desktop] › tests/e2e/webui-scale-cert.spec.ts:198:1 › desktop handles 15k torrents without rendering every row (749ms)
  -  10 [mobile] › tests/e2e/webui-visual-cert.spec.ts:171:1 › settings storage visual baseline
  ✓   9 [mobile] › tests/e2e/webui-scale-cert.spec.ts:215:1 › jumps directly to a distant row and continues within the same snapshot (649ms)
  ✓  11 [mobile] › tests/e2e/webui-cert.spec.ts:212:1 › single-torrent detail panel exposes the dock position selector (564ms)
  ✓  14 [desktop] › tests/e2e/webui-scale-cert.spec.ts:215:1 › jumps directly to a distant row and continues within the same snapshot (537ms)
  ✓  15 [mobile] › tests/e2e/webui-scale-cert.spec.ts:233:1 › core workspace controls expose accessible names (399ms)
  ✓  12 [desktop] › tests/e2e/webui-cert.spec.ts:212:1 › single-torrent detail panel exposes the dock position selector (710ms)
  -  16 [mobile] › tests/e2e/webui-cert.spec.ts:226:1 › multi-select opens an aggregate workspace and repositions the detail dock
  ✓  13 [desktop] › tests/e2e/webui-visual-cert.spec.ts:171:1 › settings storage visual baseline (849ms)
  ✓  17 [desktop] › tests/e2e/webui-scale-cert.spec.ts:233:1 › core workspace controls expose accessible names (426ms)
  ✓  18 [mobile] › tests/e2e/webui-scale-cert.spec.ts:273:1 › Ctrl+A selects the loaded page without opening add-torrent (377ms)
  ✓  20 [mobile] › tests/e2e/webui-cert.spec.ts:247:1 › mobile multi-select keeps the dock usable in side and bottom modes (614ms)
  ✓  21 [desktop] › tests/e2e/webui-scale-cert.spec.ts:273:1 › Ctrl+A selects the loaded page without opening add-torrent (376ms)
  ✓  19 [desktop] › tests/e2e/webui-cert.spec.ts:226:1 › multi-select opens an aggregate workspace and repositions the detail dock (702ms)
  -  23 [mobile] › tests/e2e/webui-cert.spec.ts:269:1 › settings storage panel renders with mocked root
  -  25 [desktop] › tests/e2e/webui-cert.spec.ts:247:1 › mobile multi-select keeps the dock usable in side and bottom modes
  ✓  22 [mobile] › tests/e2e/webui-scale-cert.spec.ts:282:1 › mouse and keyboard selection support additive and range selection (1.0s)
  ✓  26 [mobile] › tests/e2e/webui-cert.spec.ts:278:1 › mobile viewport keeps primary actions reachable (653ms)
  ✓  27 [desktop] › tests/e2e/webui-cert.spec.ts:269:1 › settings storage panel renders with mocked root (665ms)
  ✓  24 [desktop] › tests/e2e/webui-scale-cert.spec.ts:282:1 › mouse and keyboard selection support additive and range selection (968ms)
  -  28 [desktop] › tests/e2e/webui-cert.spec.ts:278:1 › mobile viewport keeps primary actions reachable
  ✓   8 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:259:1 › torrent workspace has no serious automated accessibility violations (4.8s)
  ✓   7 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:259:1 › torrent workspace has no serious automated accessibility violations (5.1s)
  ✓  29 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:275:1 › multi-selection workspace has no serious automated accessibility violations (4.1s)
  -  31 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:293:1 › settings library panel has no serious automated accessibility violations
  -  32 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:312:1 › every settings section has no serious automated accessibility violations
  -  33 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:343:1 › crash safety panel renders its status, help and save locations
  -  34 [mobile] › tests/e2e/webui-a11y-cert.spec.ts:370:1 › transient dialogs keep focus contained and have no serious automated accessibility violations
  ✓  30 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:275:1 › multi-selection workspace has no serious automated accessibility violations (4.4s)
  ✓  35 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:293:1 › settings library panel has no serious automated accessibility violations (1.0s)
  ✓  36 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:312:1 › every settings section has no serious automated accessibility violations (3.9s)
  ✓  37 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:343:1 › crash safety panel renders its status, help and save locations (1.2s)
  ✓  38 [desktop] › tests/e2e/webui-a11y-cert.spec.ts:370:1 › transient dialogs keep focus contained and have no serious automated accessibility violations (3.6s)

  10 skipped
  28 passed (20.2s)
```

Overall status: PASS
