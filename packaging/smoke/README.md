# Package Smoke

`packaging/smoke/package-smoke` validates published release and container
channels by installing or pulling from the public channel and writing
`evidence.json`, `junit.xml`, and logs under `artifacts/package-smoke/`.

TorrentNG has GitHub binary and container release channels, the Arch
`torrentngd-git` package, Fedora RPM builds through COPR, and an Ubuntu PPA at
`ppa:keefshape/torrentng`. The PPA smoke installs package `torrentngd` and
checks the installed package version against the requested release. PPA
publishing requires the Launchpad upload key secrets described in
[`../launchpad/README.md`](../launchpad/README.md). There is no separate Debian
archive channel. The PPA currently has no published packages, so its smoke
channel becomes runnable after the first signed upload.
