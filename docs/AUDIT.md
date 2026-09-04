# Integration Audit — rTorrent 0.16.x + ruTorrent 5.3.x

This file is a historical integration snapshot retained for traceability. Its
unchecked rows describe the environment and client captures that were missing
when the snapshot was written; they are not the current implementation ledger.
Current repository status, supported contracts, and remaining external gates
are maintained in [`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md)
and [`COMPATIBILITY_AUDIT.md`](COMPATIBILITY_AUDIT.md).

Status legend: ✅ confirmed working | ❌ broken | ⚠️ conditional | 🔲 not yet tested

---

## RPC Trust Model (rTorrent 0.16.9+)

rTorrent 0.16.9 introduced a trusted/untrusted XMLRPC connection model. Connections via the SCGI socket path are trusted; connections proxied through ruTorrent's httprpc plugin may be marked untrusted via `UNTRUSTED_CONNECTION` SCGI header, restricting them to a whitelisted method set.

### `load.start` via httprpc

| Client | Status | Notes |
|---|---|---|
| ruTorrent internal | ✅ | Uses trusted path |
| Prowlarr | ❌ | Hits untrusted path, `load.start not allowed` |
| Sonarr | ❌ | Same |
| Radarr | ❌ | Same |
| autobrr | ❌ | Same |
| NZB360 | ⚠️ | Read-only mostly unaffected |
| Transdrone | ⚠️ | Read-only mostly unaffected |

**Root cause:** ruTorrent#3046 — httprpc raw XMLRPC passthrough uses `UNTRUSTED_CONNECTION=1` in the default case, and the fallthrough in the trust whitelist does not include `load.start`.

**Fix:** Patch `plugins/httprpc/action.php` to mark the connection trusted when the request originates from a configured internal client, OR configure rTorrent with `rpc.trusted_connection_accept_all=true` for local socket callers only.

**Phase 1 mitigation:** Apply the httprpc patch from ruTorrent#3046 if upstream has not merged it by release time.

### `load.raw.start` via httprpc

| Status | ❌ same trust issue as `load.start` |

### `d.tracker_announce`

| Status | ❌ blocked for untrusted connections in some rTorrent 0.16.9–0.16.10 builds |
| Fix | Confirmed fixed in 0.16.11; verify with integration test |

---

## XMLRPC Backend

### xmlrpc-c vs tinyxml2

| Backend | Status | Notes |
|---|---|---|
| `--with-xmlrpc-c` | ❌ | Erratic RPC behavior reported (rtorrent#1636); Fedora package uses this |
| `--with-xmlrpc-tinyxml2` | ✅ | Preferred; build our images with this flag |

**Action:** All TorrentNG Docker images build rTorrent `--with-xmlrpc-tinyxml2`. Document in build scripts.

### Tracker HTTP user-agent control

| Field | Status | Notes |
|---|---|---|
| Runtime support | ✅ | Packaged images expose `network.http.user_agent` and `network.http.user_agent.set` |
| Upstream impact | ⚠️ | rTorrent 0.16.11 initializes libtorrent's HTTP user-agent internally, but does not publish XMLRPC commands for reading or changing it |
| TorrentNG fix | ✅ | Docker builds apply `deploy/docker/patches/rtorrent-0.16.11-user-agent-command.patch`, which wires the existing libtorrent getter/setter into rTorrent's XMLRPC command map |

### Patched bounded multicall commands

| Field | Status | Notes |
|---|---|---|
| Runtime support | ✅ | Packaged images expose `d.multicall.range` and `tng.live_summary` from `deploy/docker/patches/rtorrent-0.16.11-multicall-range.patch` |
| Sidecar calling convention | ✅ | These calls still use rTorrent's normal leading target argument. For global calls the sidecar must send an empty string as argument 0, followed by the view/range parameters. |
| Regression coverage | ✅ | `sidecar/src/rtorrent/torrents.rs` has tests that assert bounded list, nonzero-rate, and live-summary calls keep the required empty target argument. Removing it makes rTorrent return `invalid target` and the sidecar reports backend disconnected. |

### XMLRPC parsererror on torrent list

**Symptom:** ruTorrent shows "Bad response from server: (200 [parsererror,list])" (ruTorrent#2977).

| Status | ⚠️ | Intermittent; correlated with xmlrpc-c build and large torrent counts |
| Fix | Switch to tinyxml2 build; reduce d.multicall2 field count if still seen |

---

## ruTorrent 5.3.x Issues

### Plugin permission check breakage (5.3.1)

**Symptom:** Some plugins fail with permission errors after upgrade to 5.3.1.

| Status | 🔲 | Need to reproduce in test environment |
| Source | ruTorrent v5.3.1 release notes |

### PHP 8.5 deprecations

| Status | ⚠️ | ruTorrent 5.3.0 added PHP 8.2+ fixes; 8.5 may have new deprecations |
| Action | Test against PHP 8.3 (current Alpine LTS) and 8.5 if available |

### 10k+ torrent UI performance

**Symptom:** Significant slowdown in torrent table sorting with large lists.

| Status | ✅ Fixed in 5.2.10 hotfix (dxSTable sorting regression) |
| Verify | Confirm fix persists in 5.3.1 with synthetic 10k torrent load |

### Large-batch `.torrent` file add

**Symptom:** ruTorrent limits how many .torrent files can be added simultaneously.

| Status | 🔲 | Not yet quantified |
| Action | Test with 50, 100, 500 simultaneous adds |

---

## Socket and Permission Issues

### SCGI socket world-readable

**Symptom:** Socket created world-readable allows any local user to send XMLRPC.

| Status | ⚠️ | Default permissions depend on rTorrent build and umask |
| Fix | Set `umask=0007` in rtorrent.rc; run rTorrent as dedicated user; nginx/PHP in same group |

**Phase 1 mitigation:** `entrypoint.phase1.sh` runs `chmod 660 $SOCKET` after creation.

---

## Integration Test Matrix

Run these against Phase 1 build before release:

| Test | Tool | Expected | Status |
|---|---|---|---|
| Add torrent via magnet | Prowlarr | 200 OK, torrent appears | 🔲 |
| Add .torrent file | Sonarr | 200 OK, torrent appears | 🔲 |
| Add .torrent file | Radarr | 200 OK, torrent appears | 🔲 |
| Add torrent (push mode) | autobrr | torrent added and starts | 🔲 |
| List torrents | NZB360 | torrent list populated | 🔲 |
| Reannounce | ruTorrent manual | tracker announces | 🔲 |
| XMLRPC parsererror | 10k synthetic | no parsererror in logs | 🔲 |
| Socket permissions | local check | 660, not 666 | 🔲 |
| PHP 8.3 compat | ruTorrent | no deprecation warnings | 🔲 |

---

## Versions Tested

| Component | Version |
|---|---|
| rTorrent | 0.16.11 |
| libtorrent (rakshasa) | 0.16.11 |
| ruTorrent | 5.3.1 |
| XMLRPC backend | tinyxml2 |
| PHP | 8.3 |
| nginx | 1.26 |
