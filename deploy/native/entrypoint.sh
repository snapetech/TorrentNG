#!/bin/sh
set -e

TNG_INIT=/usr/bin/tini
TNG_IDENTITY_PATHS="/data /downloads /var/lib/torrentngd /run/secrets"
. /usr/local/lib/torrentng/identity.sh
tng_identity_enter "$@"
if [ "${1:-}" = --tng-identity-dropped ]; then
  shift
fi

# torrentngd only reads TORRENTNGD_CONFIG plus the standard config-file
# search path (see docs conventions) -- it has no per-field env override
# layer by design. This entrypoint bridges that to a plain `docker run` /
# Unraid workflow:
#   - default TORRENTNGD_CONFIG to /config/config.toml, a directory mount
#     point, so an empty appdata folder falls back to the packaged default
#     config baked into the image instead of failing outright.
#   - if TORRENTNGD_API_TOKEN is set, write it to the path the packaged
#     default config already expects for auth.api_tokens_file, so operators
#     can paste one token into an env var instead of bind-mounting a secret
#     file. Ignored when a custom mounted config.toml uses its own
#     auth.api_tokens / auth.api_tokens_file.

CONFIG_FILE=${TORRENTNGD_CONFIG:-/config/config.toml}

if [ ! -r "$CONFIG_FILE" ]; then
  if [ "$CONFIG_FILE" = /config/config.toml ]; then
    CONFIG_FILE=/etc/torrentngd/config.toml
  else
    echo "torrentngd config is missing or unreadable: $CONFIG_FILE" >&2
    exit 1
  fi
fi

if [ -n "${TORRENTNGD_API_TOKEN:-}" ]; then
  mkdir -p /run/secrets
  printf '%s\n' "$TORRENTNGD_API_TOKEN" > /run/secrets/torrentngd_api_token
  chmod 600 /run/secrets/torrentngd_api_token
fi

export TORRENTNGD_CONFIG="$CONFIG_FILE"
exec /usr/local/bin/torrentngd
