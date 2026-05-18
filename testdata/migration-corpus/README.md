# TorrentNG Migration Corpus

This directory is the drop point for real exported migration fixtures. The
synthetic `rt-migrate` fixture matrix covers known common aliases; this corpus is
for artifacts exported from actual client installs so undocumented key variants,
sidecar files, plugin state, and version-specific layouts are exercised.

Expected source-family directories:

| Directory | Evidence examples |
|---|---|
| `qbittorrent/` | `.fastresume`, `.torrent`, `qBittorrent.conf` |
| `transmission/` | `resume/*.resume`, `torrents/*.torrent`, settings files |
| `deluge/` | `state/torrents.state`, `.torrent` files, plugin state |
| `utorrent/` | `resume.dat`, `.torrent`, settings files |
| `biglybt/` | `downloads.config`, `torrents.config`, `.torrent` files |
| `tixati/` | config/state exports and `.torrent` files |
| `rtorrent/` | session directory files, `.torrent`, fastresume bencode |
| `generic/` | client-neutral bencoded or JSON resume edge cases |

Run:

```sh
scripts/migration_corpus_certification.sh
```

By default the gate reports `PASS_WITH_GAPS` when real corpora are missing but
the synthetic import/apply baseline passes. For a release run that must require
the exported corpus, use:

```sh
TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh
```

Use `manifest.example.toml` as the checklist for the exported client/version
families that should be attached to a release evidence bundle. When real
artifacts are present, copy it to `manifest.toml`; the certification script
validates that all required families are listed and that declared artifact paths
exist under the matching source-family directory. Each declared artifact must
include source and permission metadata so fixture provenance is reviewable, and
may include an expected `sha256` digest. In strict release mode
(`TNG_REQUIRE_MIGRATION_CORPUS=1`), `manifest.toml` is mandatory, every source
family must declare at least one artifact, and every discovered evidence file
must be declared in the manifest. The certification report includes a SHA-256
inventory for every discovered artifact.
