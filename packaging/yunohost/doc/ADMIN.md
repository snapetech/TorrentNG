# Administration

## Sign in

YunoHost restricts the WebUI page to the admins group. TorrentNG also asks for
the API password entered during installation. The username field accepts any
value; use the API password in the password field. The same token is required
for direct API access.

The main permission protects the WebUI. The `/api` permission is available to
visitors so local automation clients can reach the API without YunoHost SSO;
TorrentNG still requires its API token. Keep the token private.

## Native mode

The default payload directory is
`/home/yunohost.multimedia/share/TorrentNG`. New files are owned by TorrentNG
and use the `multimedia` group so other media apps can read them. Payloads are
outside the YunoHost app data directory: they are excluded from app backups and
remain on disk after uninstall.

YunoHost opens the selected peer port for TCP and UDP in its firewall. Forward
that same port on the router for inbound peer connections. DHT uses the same
port. The application API remains bound to localhost behind YunoHost NGINX.

## Existing-client mode

Configure the selected backend first and enter the address and credentials it
expects. qBittorrent, Transmission, and Deluge must expose their Web API only
to trusted local clients. rTorrent SCGI must remain on a Unix socket or a
trusted private network; SCGI has no authentication. TorrentNG stores the
connection settings in `$data_dir/config.toml`, readable only by the app user.

The storage root tells TorrentNG where the existing backend's local files are
visible on this server. The default is `/home/yunohost.multimedia/share`. Use a
path that matches the selected backend's save paths. Remote backends whose
files are not mounted locally can still be controlled, but local file browsing
and storage views need a matching local path.

To change the backend or credentials later, edit `$data_dir/config.toml` and
restart the `torrentng` service. The `backend.type` value is one of
`qbittorrent`, `transmission`, `deluge`, `rtorrent`, or `torrentng`.

## Backups and upgrades

YunoHost backs up the app binaries, WebUI, engine session/database state, and
sidecar cache/configuration. Downloaded payload files are kept outside app
state and are not included. Upgrades replace binaries and WebUI assets while
preserving app state and payloads.

After changing the app URL, the package updates the WebUI API and WebSocket
prefix and restarts the service. Root and subpath installs are supported.
