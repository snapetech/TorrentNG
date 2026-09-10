# Public Torrent Transfer and Soak — 2026-09-05

Status: **COMPLETE / PASS**
Confidence: **high** for both the public transfer and the counted soak; the
finalizer accepted the completed source report and all configured checks.

## Public transfer

The public matrix resolved the official Debian BitTorrent source and passed a
real transfer through the Docker interop stack:

| Field | Value |
| --- | --- |
| Source page | <https://www.debian.org/CD/torrent-cd/> |
| Resolved metainfo | <https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/debian-13.6.0-amd64-netinst.iso.torrent> |
| Torrent name | `debian-13.6.0-amd64-netinst.iso` |
| v1 info hash | `481b6e3617be4c88f96cb25e47c9d8272130071e` |
| Total bytes | `791674880` |
| Matrix report | [`interop-matrix-20260910T192200Z.md`](../certification/reports/interop-matrix-20260910T192200Z.md) |
| Matrix result | **PASS** at b393; Rust completed and observed 142 peers across all five configured clients |

The host resolved and downloaded the metainfo, then the harness supplied that
file to Rust, qBittorrent, Transmission, Deluge, and rTorrent. The current
b393 all-live parent record is [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md).
This avoids
turning a client-container metadata-DNS failure into a transfer result. The
interop Compose services now use overrideable explicit DNS servers
(`INTEROP_DNS_PRIMARY`, `INTEROP_DNS_SECONDARY`).

## Completed 24-hour soak

The counted soak started at `2026-09-05T19:32:58Z` under the user-systemd unit
`torrentng-public-debian-soak-20260905-v3.service` with:

- target: `http://127.0.0.1:28180`
- daemon container: `torrentng-interop-torrentngd-1`
- expected torrent name: `debian-13.6.0-amd64-netinst.iso`
- expected torrent hash: `481b6e3617be4c88f96cb25e47c9d8272130071e`
- duration: `86400` seconds
- interval: `60` seconds
- initial MainPID: `2230423` (the systemd unit owns process lifetime and does not auto-restart a failed soak)
- live report: `.run/soak-24h-public-debian-20260905-v3.md`
- live log: `.run/soak-24h-public-debian-20260905-v3.log`

The run completed with 1,437 retained samples. Every sample returned healthy
`/health`, qBittorrent `sync/maindata`, and metrics responses; the exact
expected Debian torrent remained present and completed; and the configured RSS,
file-descriptor, thread, and disk-free ceilings passed. The final observed
resource maxima/floor were 1.3 MB RSS, 3 file descriptors, 1 thread, and
14,382 MB free disk.

The counted source report is [`soak-24h-public-debian-20260905-v3.md`](../.run/soak-24h-public-debian-20260905-v3.md), and the checked finalization report
is [`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md).
The finalizer required at least 1,200 samples and one matching torrent; it
recorded `Overall status: PASS`.

The soak source predates the b393 code/evidence refresh; the current b393
public matrix above is the artifact-specific transfer evidence. The user-systemd unit owned the process and had automatic restart disabled, so
a crash would have remained visible rather than being replaced by a fresh
run. The unit was collected after completion; its absence now is expected.

The final source tail is available for audit:

```sh
tail -n 20 .run/soak-24h-public-debian-20260905-v3.md
SOAK_MIN_TORRENTS=1 scripts/soak_status.sh .run/soak-24h-public-debian-20260905-v3.md
```

The first background-launch attempt was terminated after its first sample by
the agent shell and is not counted as soak evidence. The counted run used the
systemd path directly. Automatic restart was disabled so a process failure
would have remained visible in the report.
