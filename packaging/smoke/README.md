# Package Smoke

`packaging/smoke/package-smoke` validates published release and container
channels by installing or pulling from the public channel and writing
`evidence.json`, `junit.xml`, and logs under `artifacts/package-smoke/`.

TorrentNG currently has release binary and container channels, the Arch
`torrentngd-git` package, and RPM builds published through COPR. The checked-in
smoke configuration exercises the channels with public release artifacts;
Debian and PPA channels are not enabled.
