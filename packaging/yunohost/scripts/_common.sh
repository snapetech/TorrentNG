#!/bin/bash

source /usr/share/yunohost/helpers

package_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

write_runtime_config() {
	python3 "$package_dir/conf/write-runtime-config.py" \
		"$install_dir/webui/runtime-config.js" "$1"
	chown "$app:$app" "$install_dir/webui/runtime-config.js"
	chmod 0644 "$install_dir/webui/runtime-config.js"
}

write_app_config() {
	local mode="$1"
	local config_path="$data_dir/config.toml"
	local session_secret="$2"
	local api_token="$3"

	TNG_DEPLOYMENT_MODE="$mode" \
	TNG_DATA_DIR="$data_dir" \
	TNG_HTTP_PORT="$port" \
	TNG_PEER_PORT="$port_peer" \
	TNG_DOWNLOAD_DIR="$download_dir" \
	TNG_STORAGE_ROOT="${storage_root:-}" \
	TNG_BACKEND_TYPE="${backend_type:-}" \
	TNG_SESSION_SECRET="$session_secret" \
	TNG_API_TOKEN="$api_token" \
	TNG_QBITTORRENT_URL="${qbittorrent_url:-}" \
	TNG_QBITTORRENT_USERNAME="${qbittorrent_username:-}" \
	TNG_QBITTORRENT_PASSWORD="${qbittorrent_password:-}" \
	TNG_TRANSMISSION_URL="${transmission_url:-}" \
	TNG_TRANSMISSION_USERNAME="${transmission_username:-}" \
	TNG_TRANSMISSION_PASSWORD="${transmission_password:-}" \
	TNG_DELUGE_URL="${deluge_url:-}" \
	TNG_DELUGE_PASSWORD="${deluge_password:-}" \
	TNG_RTORRENT_TRANSPORT="${rtorrent_transport:-}" \
	TNG_RTORRENT_SOCKET="${rtorrent_socket:-}" \
	TNG_RTORRENT_SCGI_ADDR="${rtorrent_scgi_addr:-}" \
	TNG_TORRENTNG_URL="${torrentng_url:-}" \
	TNG_TORRENTNG_API_TOKEN="${torrentng_api_token:-}" \
	python3 "$package_dir/conf/write-config.py" "$config_path"
	chown "$app:$app" "$config_path"
	chmod 0600 "$config_path"
}

initialize_multimedia_access() {
	if [[ -L "$download_dir" ]]; then
		ynh_die --message="The download directory must not be a symbolic link."
	fi
	if [[ ! -e "$download_dir" ]]; then
		install -d -o "$app" -g multimedia -m 2775 "$download_dir"
	elif [[ ! -d "$download_dir" ]]; then
		ynh_die --message="The download path exists but is not a directory."
	elif ! ynh_exec_as "$app" test -w "$download_dir"; then
		ynh_die --message="The existing download directory is not writable by TorrentNG. Set group access for $app or choose another directory."
	fi
}

validate_storage_root() {
	if [[ -z "${storage_root:-}" ]]; then
		return
	fi
	case "$storage_root" in
		/*) ;;
		*) ynh_die --message="The storage root must be an absolute filesystem path or empty." ;;
	esac
	if [[ ! -d "$storage_root" ]]; then
		ynh_die --message="The storage root does not exist. Use an existing local path or leave it empty for a remote backend."
	fi
	if ! ynh_exec_as "$app" test -r "$storage_root" || ! ynh_exec_as "$app" test -x "$storage_root"; then
		ynh_die --message="The TorrentNG service user cannot read the storage root. Grant $app read and directory-traversal access or choose another path."
	fi
}

install_launch_script() {
	install -o "$app" -g "$app" -m 0755 \
		"$package_dir/scripts/launch" "$install_dir/bin/launch"
}

reconcile_peer_firewall() {
	if [[ "$deployment_mode" == "native" ]]; then
		yunohost firewall allow Both "$port_peer"
		ynh_app_setting_set --app="$app" --key=peer_firewall_open --value=1
	elif [[ "${peer_firewall_open:-0}" == "1" && -n "${port_peer:-}" ]]; then
		yunohost firewall disallow Both "$port_peer" || true
		ynh_app_setting_set --app="$app" --key=peer_firewall_open --value=0
	fi
}
