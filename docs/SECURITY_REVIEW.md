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
- Set `allowed_script_dirs` to one or more root-owned directories. Do not leave it empty in production when scripts are enabled.
- Make scripts root-owned or service-owner-owned, non-world-writable, and executable only by trusted users.
- Keep `script_timeout_secs` low enough that a stuck post-complete job cannot exhaust worker capacity.
- Treat workflow script output as operational logs, not user-facing content.
- Use API tokens with the minimum distribution necessary for automation clients.

Runtime behavior:

- The sidecar refuses script actions unless `allow_scripts` is true.
- When `allowed_script_dirs` is non-empty, the script path is canonicalized and must live under one of those directories.
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
`rusttorrentd` configs do not use a session secret, so the same script records
that check as not applicable and still enforces API-token review.

## Release Checklist

- [ ] Run `scripts/security_review.sh` against the release config.
- [ ] Confirm script workflows are disabled, or allowed directories are explicit and non-world-writable.
- [ ] Confirm API tokens are not default/example values.
- [ ] Confirm reverse proxy auth only uses `trust_proxy_header = true` behind a trusted proxy that strips inbound spoofed headers.
- [ ] Confirm `/metrics` exposure is either internal-only or protected by the deployment network.
- [ ] Attach the generated security report to the release notes.
