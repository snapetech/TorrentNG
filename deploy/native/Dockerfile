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

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/torrentngd --create-home --shell /usr/sbin/nologin torrentngd \
    && mkdir -p /data /config /var/lib/torrentngd /usr/share/torrentng/webui \
    && chown -R torrentngd:torrentngd /data /config /var/lib/torrentngd

COPY --from=build /src/target/release/torrentngd /usr/local/bin/torrentngd
COPY --from=webui-build /webui/dist /usr/share/torrentng/webui
COPY deploy/native/config.toml /etc/torrentngd/config.toml

ENV TORRENTNGD_CONFIG=/etc/torrentngd/config.toml
ENV TNG_STATIC_DIR=/usr/share/torrentng/webui
ENV RUST_LOG=info

USER torrentngd
VOLUME ["/data", "/var/lib/torrentngd", "/config"]
EXPOSE 8080 44444 44444/udp

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/torrentngd"]
CMD []
