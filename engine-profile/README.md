# engine-profile/

Compatible-client integration only. This is the pinned rTorrent/libtorrent
build and tuning profile used by rTorrent-backed TorrentNG WebUI/API service
deployments. It has no bearing on the TorrentNG client (`torrentngd`); if
TorrentNG owns transfer and storage, nothing in this directory applies.

## What's here

| Path | Purpose |
|---|---|
| `rtorrent.rc` | Baseline rTorrent config: SCGI socket setup, DHT/port settings, and the trusted-connection model the compatible-client service relies on |
| `build/` | Build helper and pinned version defaults for the rTorrent/libtorrent + tinyxml2 known-good bundle (see [build/README.md](build/README.md)) |

## Why it's pinned

rTorrent 0.16.9+ introduced a trusted/untrusted XMLRPC connection model that
blocks calls like `load.start` over untrusted connections, and the upstream
`xmlrpc-c` build path is erratic — both have broken Prowlarr, Sonarr, Radarr,
and other automation clients in the wild. Pinning the rTorrent/libtorrent
build with `--with-xmlrpc-tinyxml2` and shipping a known-good `rtorrent.rc`
keeps that automation working reliably against the compatible-client service.

## Related

- [deploy/docker/](../deploy/docker/) — the Compose stacks that consume this
  profile (`compose.yml` for the compatible-client service, `compose.phase1.yml` for the raw
  rTorrent/ruTorrent bundle)
- [docs/DEPLOYMENT.md](../docs/DEPLOYMENT.md) — Track 1 deployment guide
- [docs/TRACKER-IDENTITY.md](../docs/TRACKER-IDENTITY.md) — user-agent/peer-id
  policy enforced on top of this profile
