# Security Review

## Script Workflows

Script workflow execution is disabled by default and must be explicitly enabled:

```toml
[workflows]
allow_scripts = true
script_timeout_secs = 30
allowed_script_dirs = ["/etc/torrentng/workflows"]
```

Required production policy:

- Keep `allow_scripts = false` unless the deployment owner has reviewed every script.
- Set `allowed_script_dirs` to one or more root-owned directories. Configuration is rejected if scripts are enabled without an allowlist.
- Make scripts root-owned or service-owner-owned, non-world-writable, and executable only by trusted users.
- Keep `script_timeout_secs` low enough that a stuck post-complete job cannot exhaust worker capacity.
- Treat workflow script output as operational logs, not user-facing content.
- Use API tokens with the minimum distribution necessary for automation clients.

Runtime behavior:

- The sidecar refuses script actions unless `allow_scripts` is true.
- Script commands must use an absolute executable path. The path is canonicalized before launch and must live under one of the configured allowlist directories. Workflow webhooks use address-pinned, no-redirect HTTP with bounded responses and reject private/local destinations unless `allow_private_webhooks` is explicitly enabled.
- The sidecar passes torrent context through environment variables instead of interpolating values into the command:
  - `TNG_WORKFLOW_ID`
  - `TNG_WORKFLOW_NAME`
  - `TNG_TORRENT_HASH`
  - `TNG_CATEGORY`
  - `TNG_TRACKER`
- The sidecar enforces a timeout and records success/failure in workflow run history.

## Automated Review

Run the review against the deployment config that will be shipped:

```sh
TNG_SECRET_KEY="$(openssl rand -hex 32)" TNG_API_TOKENS="token-one,token-two" \
  scripts/security_review.sh deploy/docker/sidecar.config.toml
scripts/security_review.sh deploy/native/config.toml
```

Sidecar configs must provide a non-example `secret_key`. Native
`torrentngd` configs do not use a session secret, so the same script records
that check as not applicable and still enforces API-token review.

## Current local review (2026-09-04)

`scripts/security_review.sh` was run against the native and sidecar deployment
configs with generated non-placeholder tokens. It passed the config, token,
and script-policy checks. Dependency resolution and image scanning are recorded
separately by `scripts/security_scan.sh`. The exact reports are
[`security-review-native-current-20260904.md`](../certification/reports/security-review-native-current-20260904.md)
and
[`security-review-sidecar-current-20260904.md`](../certification/reports/security-review-sidecar-current-20260904.md).

Native Prometheus hot-torrent labels now hash infohashes by default. Raw
identifiers require `metrics.include_torrent_ids = true`, and startup emits a
warning when that opt-in is used. `/metrics` remains behind the native auth
middleware when tokens are configured. The sidecar now applies the same token
gate to `/metrics`; deployments must still keep the route on an
internal/protected network.

The sidecar's `trust_proxy_header` mode is loopback-only. A reverse proxy must
strip inbound `X-Remote-User` values before forwarding; public binds fail
configuration validation when this mode is enabled.

Remaining deployment review is operator-owned: verify the actual reverse proxy
strips spoofable identity headers, the rendered secret is non-placeholder, and
the deployed metrics route is not publicly exposed. Those facts cannot be
proved from this repository's static config templates.
