FROM rust:1.96-alpine AS build

RUN apk add --no-cache build-base linux-headers musl-dev

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --target x86_64-unknown-linux-musl -p torrentngd

FROM node:22-bookworm-slim@sha256:48e4b67d85f87bd551df43704e24d252f56cc5f8e9718841aace50f19948f0f9 AS webui-build

WORKDIR /webui
COPY webui/package.json webui/package-lock.json ./
RUN npm ci
COPY webui/ ./
RUN TNG_WEBUI_OUT_DIR=dist npm run build

FROM alpine:3.24.2
ARG TNG_UID=1000
ARG TNG_GID=1000
ARG TORRENTNG_SOURCE_REVISION=unknown

RUN apk add --no-cache ca-certificates curl jq tini util-linux \
    && case "$TNG_UID" in ''|*[!0-9]*) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && case "$TNG_GID" in ''|*[!0-9]*) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && mkdir -p /data /downloads /config /var/lib/torrentngd /usr/share/torrentng/webui /run/secrets \
    && chown -R "$TNG_UID:$TNG_GID" /data /downloads /config /var/lib/torrentngd /run/secrets

COPY --from=build /src/target/x86_64-unknown-linux-musl/release/torrentngd /usr/local/bin/torrentngd
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
      org.opencontainers.image.description="TorrentNG Engine: native Rust BitTorrent client with the shared WebUI/API, migration tooling, storage, jobs, and recovery." \
      org.opencontainers.image.revision="${TORRENTNG_SOURCE_REVISION}"

USER root
VOLUME ["/data", "/downloads", "/var/lib/torrentngd", "/config"]
EXPOSE 8080 44444 44444/udp

ENTRYPOINT ["/entrypoint.sh"]
CMD []
