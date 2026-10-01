## First sign-in

Open the TorrentNG tile in YunoHost. Sign in with any username and the API
password entered during installation.

In native mode, YunoHost's firewall is configured for the selected peer port.
Forward that TCP and UDP port on your router if you want peers outside your
local network to connect directly. Downloaded files are stored in
`/home/yunohost.multimedia/share/TorrentNG` by default.

In existing-client mode, ensure the selected client is running and reachable
from the YunoHost host. For rTorrent, the configured SCGI socket or address
must be accessible to the TorrentNG system user.
