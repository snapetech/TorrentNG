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
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/* \
    && case "$TNG_UID" in ''|*[!0-9]*) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_UID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && case "$TNG_GID" in ''|*[!0-9]*) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; *[1-9]*) :;; *) echo "TNG_GID must be a nonzero numeric ID" >&2; exit 1;; esac \
    && groupadd --system --gid "$TNG_GID" torrentngd \
    && useradd --system --uid "$TNG_UID" --gid "$TNG_GID" --home /var/lib/torrentngd --create-home --shell /usr/sbin/nologin torrentngd \
    && mkdir -p /data /config /var/lib/torrentngd /usr/share/torrentng/webui /run/secrets \
    && chown -R torrentngd:torrentngd /data /config /var/lib/torrentngd /run/secrets

COPY --from=build /src/target/release/torrentngd /usr/local/bin/torrentngd
COPY --from=webui-build /webui/dist /usr/share/torrentng/webui
COPY deploy/native/config.toml /etc/torrentngd/config.toml
COPY deploy/native/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV TNG_STATIC_DIR=/usr/share/torrentng/webui
ENV RUST_LOG=info

LABEL org.opencontainers.image.title="TorrentNG Engine" \
      org.opencontainers.image.description="TorrentNG Engine: native Rust BitTorrent client with the shared WebUI/API, migration tooling, storage, jobs, and recovery."

USER torrentngd
VOLUME ["/data", "/var/lib/torrentngd", "/config"]
EXPOSE 8080 44444 44444/udp

ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
CMD []
