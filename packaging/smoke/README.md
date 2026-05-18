# Package Smoke

`packaging/smoke/package-smoke` validates published release and container
channels by installing or pulling from the public channel and writing
`evidence.json`, `junit.xml`, and logs under `artifacts/package-smoke/`.

TorrentNG currently has release binary/container and Arch packaging surfaces.
Debian, RPM, PPA, and COPR channels should be added only after package metadata
exists.
