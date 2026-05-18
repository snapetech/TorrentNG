# TorrentNG Migration Corpus

This directory contains a checked-in generated fixture corpus for deterministic
strict local gates, and is also the drop point for additional real exported
migration fixtures. The synthetic `rt-migrate` fixture matrix covers known
common aliases; generated artifacts here keep every source-family directory and
manifest path exercised in CI. Real client exports can be added beside these
fixtures when release evidence needs undocumented key variants, sidecar files,
plugin state, and version-specific layouts from actual installs.

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

The checked-in generated manifest lets strict local corpus validation pass. For
a release run that must require the corpus manifest and all declared artifacts,
use:

```sh
TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh
```

Use `manifest.example.toml` as the checklist for extra exported client/version
families that should be attached to a release evidence bundle. Every declared
artifact must include source and permission metadata so fixture provenance is
reviewable, and may include an expected `sha256` digest. In strict release mode
(`TNG_REQUIRE_MIGRATION_CORPUS=1`), `manifest.toml` is mandatory, every source
family must declare at least one artifact, and every discovered evidence file
must be declared in the manifest. The certification report includes a SHA-256
inventory for every discovered artifact.
