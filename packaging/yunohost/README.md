# TorrentNG for YunoHost

This package provides two install-time modes:

- **TorrentNG engine and WebUI** runs the native `torrentngd` transfer engine.
- **WebUI/API for an existing torrent client** runs the `torrentng` sidecar and connects to an existing qBittorrent, Transmission, Deluge, rTorrent, or TorrentNG engine.

The package uses YunoHost packaging format 2. It supports YunoHost 12 on
`amd64` and `arm64`; the release bundles use static musl binaries so they do
not depend on the host glibc version. The WebUI supports root and subpath
installs, and the package keeps its state separate from downloaded payloads.

The upstream release workflow builds the architecture-specific bundles named
in `manifest.toml`. The app catalog is a separate YunoHost repository; the
package should be listed there only after the release assets and YunoHost
package checks are available.

## Local package check

Install the current package branch directly with YunoHost's package tooling:

```sh
sudo yunohost app install https://github.com/snapetech/torrentng_ynh --debug
```

The `tests.toml` file is intentionally minimal. The official package CI should
exercise install, backup, restore, upgrade, and removal against a YunoHost VM.
