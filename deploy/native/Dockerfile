FROM rust:1.96-bookworm AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p torrentngd

FROM node:22-bookworm-slim AS webui-build

WORKDIR /webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci
COPY webui/ ./
RUN TNG_WEBUI_OUT_DIR=dist npm run build

FROM debian:bookworm-slim
ARG TNG_UID=1000
ARG TNG_GID=1000

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl jq tini util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && case "$TNG_UID" in ''|*[!0-9]*) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && case "$TNG_GID" in ''|*[!0-9]*) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && if ! getent group "$TNG_GID" >/dev/null; then groupadd --system --gid "$TNG_GID" torrentngd; fi \
    && if ! getent passwd "$TNG_UID" >/dev/null; then useradd --system --uid "$TNG_UID" --gid "$TNG_GID" --home /var/lib/torrentngd --create-home --shell /usr/sbin/nologin torrentngd; fi \
    && mkdir -p /data /config /var/lib/torrentngd /usr/share/torrentng/webui /run/secrets \
    && chown -R "$TNG_UID:$TNG_GID" /data /config /var/lib/torrentngd /run/secrets

COPY --from=build /src/target/release/torrentngd /usr/local/bin/torrentngd
COPY --from=webui-build /webui/dist /usr/share/torrentng/webui
COPY deploy/native/config.toml /etc/torrentngd/config.toml
COPY deploy/container/identity.sh /usr/local/lib/torrentng/identity.sh
COPY deploy/native/vpn-port-sync.sh /usr/local/lib/torrentng/vpn-port-sync.sh
COPY deploy/native/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh /usr/local/lib/torrentng/vpn-port-sync.sh

ENV TNG_STATIC_DIR=/usr/share/torrentng/webui
ENV RUST_LOG=info
ENV TNG_DEFAULT_UID="${TNG_UID}"
ENV TNG_DEFAULT_GID="${TNG_GID}"

LABEL org.opencontainers.image.title="TorrentNG Engine" \
      org.opencontainers.image.description="TorrentNG Engine: native Rust BitTorrent client with the shared WebUI/API, migration tooling, storage, jobs, and recovery."

USER root
VOLUME ["/data", "/var/lib/torrentngd", "/config"]
EXPOSE 8080 44444 44444/udp

ENTRYPOINT ["/entrypoint.sh"]
CMD []
