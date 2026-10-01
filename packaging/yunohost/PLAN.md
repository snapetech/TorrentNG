# YunoHost packaging and publication plan

## Package shape

Maintain one YunoHost package with a required install-time mode:

1. **Native:** run `torrentngd`, the WebUI, and the BitTorrent engine.
2. **Existing client:** run `torrentng`, the WebUI/API sidecar, and connect it to qBittorrent, Transmission, Deluge, rTorrent, or another TorrentNG engine.

This matches TorrentNG's component split in the upstream repo. The package
must not install a second torrent engine in existing-client mode.

## Build and runtime

- Build one Linux/musl archive per supported YunoHost architecture (`amd64`,
  `arm64`) from the upstream release commit. Include both Rust executables and
  the production WebUI assets. Dedicated `yunohost-*` tags publish the first
  app bundles; normal `main-*` releases also publish updated bundles. Publish
  SHA256 checksums with both release types. This avoids depending on the host's
  glibc version or a build toolchain during app installation.
- Keep app binaries and static assets under `install_dir`; keep engine state,
  sidecar cache, and credentials under `data_dir`.
- Keep native payloads outside app state in the YunoHost multimedia share.
  Use the `multimedia` group for shared access. Do not include payloads in app
  backups or delete them on app removal.
- Bind the service to loopback, reserve a YunoHost proxy port, and expose the
  peer TCP/UDP port only in native mode. Existing-client mode must not claim or
  open an inbound peer port.
- Serve through YunoHost NGINX with WebSocket upgrade support, a 100 MB request
  limit, cookie path scoping, and root/subpath URL support. Protect the WebUI
  with the YunoHost admins permission; allow `/api` through SSOwat for client
  integrations while requiring TorrentNG's own API token.
- Run as the dedicated app user with systemd filesystem/network restrictions.

## Lifecycle and data

- **Install:** validate the API token, fetch the verified architecture bundle,
  write mode-specific TOML with correctly escaped values, generate a private
  session secret for sidecar mode, configure NGINX/systemd/firewall, and start
  the service.
- **Upgrade:** replace only packaged binaries/assets, preserve configuration
  and app state, regenerate URL-aware WebUI configuration, refresh service
  integration, and restart.
- **URL change:** rewrite the runtime WebUI prefix and NGINX config, then
  restart so API requests and WebSockets continue beneath the new path.
- **Backup/restore:** include package files, configuration, engine session and
  database, and sidecar cache. Exclude large payloads. Recreate NGINX/systemd
  and conditional firewall settings during restore.
- **Removal:** stop the service and remove NGINX/systemd integration. Close a
  native peer firewall rule. Retain payloads; let YunoHost apply its normal
  data-directory purge choice to app state.

## Validation gates

Before catalog submission:

1. Parse/lint the v2 manifest and shell/Python scripts.
2. Build WebUI and both Rust binaries for amd64 and arm64; inspect the binaries
   to confirm they do not require host glibc.
3. Run `scripts/yunohost_bundle_smoke.sh` against each architecture bundle. It
   starts the native engine and the existing-client sidecar, connects the
   sidecar to the engine, and checks health, token login, protected API access,
   static assets, and the subpath runtime configuration.
4. Verify root and subpath API, WebSocket, cookie, SSO permission, and native
   peer-firewall behavior on a YunoHost 12 host or official package-check VM.
5. Run official app package CI for install, backup/restore, upgrade, URL change,
   and removal in both install modes.
6. Publish a separate YunoHost package repository with release URLs and SHA256
   values, then submit its catalog entry and logo to `YunoHost/apps`. Keep the
   catalog state `inprogress` until YunoHost's own package CI passes.

See the [YunoHost v2 packaging guide](https://doc.yunohost.org/dev/packaging/advanced/packaging_v2/),
[resource guide](https://doc.yunohost.org/en/dev/packaging/resources/), and
[catalog publication guide](https://doc.yunohost.org/dev/packaging/publish/).
