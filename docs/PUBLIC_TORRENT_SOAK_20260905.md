# Public Torrent Transfer and Soak — 2026-09-05

Status: **IN_PROGRESS**
Confidence: **high** for the public transfer result; **unknown** for the
24-hour result until the supervised run finishes.

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
| Matrix report | [`public-debian-interop-20260905T191253Z.md`](../certification/reports/public-debian-interop-20260905T191253Z.md) |
| Matrix result | **PASS**; Rust completed and observed 3 reference-client peers |

The host resolved and downloaded the metainfo, then the harness supplied that
file to Rust, qBittorrent, Transmission, Deluge, and rTorrent. This avoids
turning a client-container metadata-DNS failure into a transfer result. The
interop Compose services now use overrideable explicit DNS servers
(`INTEROP_DNS_PRIMARY`, `INTEROP_DNS_SECONDARY`).

## Active 24-hour soak

The soak started at `2026-09-05T19:22:10Z` under the user-systemd unit
`torrentng-public-debian-soak-20260905.service` with:

- target: `http://127.0.0.1:28180`
- daemon container: `torrentng-interop-torrentngd-1`
- expected torrent name: `debian-13.6.0-amd64-netinst.iso`
- expected torrent hash: `481b6e3617be4c88f96cb25e47c9d8272130071e`
- duration: `86400` seconds
- interval: `60` seconds
- initial MainPID: `2189470` (the systemd unit owns restart behavior)
- live report: `.run/soak-24h-public-debian-20260905-v2.md`
- live log: `.run/soak-24h-public-debian-20260905-v2.log`

The first sample was healthy: `/health` 200, one torrent, 100% completed,
RSS 1.3 MB, one thread, three file descriptors, 20,632 MB free, metrics 200,
and database/cache/storage health fields healthy. The soak script now checks
the exact public torrent identity and completed uploading/seeding state on
every sample. It will not report PASS until the full duration completes.

Inspect the supervisor and live report:

```sh
systemctl --user status torrentng-public-debian-soak-20260905.service
tail -n 20 .run/soak-24h-public-debian-20260905-v2.md
SOAK_MIN_TORRENTS=1 scripts/soak_status.sh .run/soak-24h-public-debian-20260905-v2.md
```

The first background-launch attempt was terminated after its first sample by
the agent shell and is not counted as soak evidence. The launcher now prefers
a user-systemd supervisor when available and falls back to a detached session
when it is not; the active run uses the systemd path directly.
