# Unraid deployment

Unraid Community Applications (CA) templates for TorrentNG. See
[deploy/README.md](../README.md) for the underlying Docker images and
[docs/DEPLOYMENT.md](../../docs/DEPLOYMENT.md) /
[docs/NATIVE_DEPLOYMENT.md](../../docs/NATIVE_DEPLOYMENT.md) for the
non-Unraid deployment paths these templates wrap.

## Which template

- Already running rTorrent, qBittorrent, Transmission, or Deluge (on this
  box or elsewhere on the network) and just want a modern replacement WebUI
  and a qBittorrent-compatible API for Prowlarr/Sonarr/Radarr/autobrr/
  cross-seed? Use **torrentng-webui**.
- Want one box that does everything with no external client dependency? Use
  **torrentng** (runs `torrentngd`, TorrentNG's own first-party engine).

Both are documented in depth in their own `<Overview>` text, which Unraid
shows on the Add Container page.

## WebUI login

Both TorrentNG profiles default to `torrentng` / `torrentng` on loopback
installs. The Unraid templates publish on a public container bind, so their
default password is disabled: use the template's **API Token** or **API
Tokens** value in either the username or password field. After signing in,
open **Settings -> Security** to set a unique username and password. API
tokens remain available for automation. You can also configure credentials in
`[auth].username` and `[auth].password`; runtime changes persist in the
profile's state directory until restored from config.toml.

## Native torrentng behind Gluetun

The native daemon can share an existing Gluetun network namespace and follow
the provider-assigned port after every VPN reconnect. Use the
[VPN Compose variant](../native/compose.vpn.yml) when managing the deployment
with Compose. For the Community Applications template:

1. Configure Gluetun with `VPN_PORT_FORWARDING=on` and its authenticated
   control server, for example
   `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE={"auth":"apikey","apikey":"..."}`.
   Publish the TorrentNG WebUI/API port (`8080`) on the Gluetun container; do
   not expose its control-server port to the host.
2. In the `torrentng` template, set Docker Extra Parameters to
   `--network=container:gluetun` and remove the WebUI and peer-port mappings
   from the TorrentNG container. Its shared network is published through
   Gluetun.
3. Start a port-sync helper in the same network namespace. The native image
   already contains the helper:

   ```sh
   docker run -d --name=torrentng-vpn-port-sync \
     --restart=unless-stopped \
     --network=container:gluetun \
     --user=99:100 \
     -e GLUETUN_CONTROL_API_KEY='same-key-configured-in-gluetun' \
     -e TORRENTNGD_API_TOKEN='same-token-configured-in-torrentng' \
     --entrypoint=/usr/local/lib/torrentng/vpn-port-sync.sh \
     ghcr.io/snapetech/torrentng/native:latest
   ```

The helper polls Gluetun's authenticated `/v1/portforward` endpoint and
updates TorrentNG's runtime API. The daemon rebinds TCP and enabled uTP, saves
the assigned port, and refreshes its tracker/DHT peer-port announcements.
Gluetun provides the WireGuard kill switch and provider NAT-PMP forwarding;
for ProtonVPN, enable NAT-PMP when generating the WireGuard profile. Use the
provider's supported port-forwarding setup and a separate key for Gluetun's
control server. Keep `dht.port = 0` so DHT follows the runtime peer port; with
incoming uTP enabled, TorrentNG uses the adjacent UDP port for DHT. See
[NATIVE_DEPLOYMENT.md](../../docs/NATIVE_DEPLOYMENT.md) for the complete
Compose example.

## Manual template installation

For a manual installation, use the template for the mode you want:

1. Docker tab -> **Add Container**.
2. In the **Template** field at the top, paste the raw GitHub URL of the
   template you want, e.g.
   `https://raw.githubusercontent.com/snapetech/TorrentNG/main/templates/torrentng-webui.xml`.
3. The form populates from the template. Fill in the required fields
   (Secret Key, API Tokens, and the backend URL/credentials for
   `torrentng-webui`; the API Token and Config path for `torrentng`) and
   Apply.

For the native engine, use
[`templates/torrentng.xml`](https://raw.githubusercontent.com/snapetech/TorrentNG/main/templates/torrentng.xml).

## Storage and permissions

Both templates default to Unraid's `nobody:users` identity (`PUID=99`,
`PGID=100`). Set **PUID** and **PGID** to an identity that can read and write
its appdata and downloads. Preserve your existing download client's access
when sharing folders. Use these variables rather than a Docker `--user`
override, because the container entrypoint needs to prepare its runtime files
before dropping privileges.

For the WebUI template, configure the selected backend's URL and credentials.
When using rTorrent, the SCGI address must be reachable before startup; the
service exits if it cannot apply its tracker identity. Other supported
backends report degraded health while reconnecting.

## Support

Use the [TorrentNG issue tracker](https://github.com/snapetech/TorrentNG/issues)
for installation and runtime problems. Include your template choice, image
version, selected backend, and relevant logs with credentials removed.
