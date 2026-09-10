# TorrentNG Security Review

- Date UTC: 2026-09-10T19:34:22Z
- Config: /home/keith/Documents/code/TorrentNG/deploy/native/config.toml

## Automated Checks

| Check | Result | Detail |
|---|---|---|
| script workflows default | PASS | disabled |
| api tokens | PASS | non-empty and not a known example |
| session secret | PASS | not applicable for native API-token-only config |
| proxy header trust | PASS | disabled |

## Manual Checks

- Confirm script directories are owned by the service owner or root and are not world-writable.
- Confirm metrics is exposed only to trusted networks; /health is intentionally public for probes.
- Confirm Docker/systemd deployments do not mount workflow script directories writable from untrusted paths.

Overall status: PASS
