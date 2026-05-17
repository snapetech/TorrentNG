FROM rust:1.93-bookworm AS build

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p rusttorrentd

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/rusttorrentd --create-home --shell /usr/sbin/nologin rusttorrentd \
    && mkdir -p /data /config /var/lib/rusttorrentd \
    && chown -R rusttorrentd:rusttorrentd /data /config /var/lib/rusttorrentd

COPY --from=build /src/target/release/rusttorrentd /usr/local/bin/rusttorrentd
COPY deploy/native/config.toml /etc/rusttorrentd/config.toml

ENV RUSTTORRENTD_CONFIG=/etc/rusttorrentd/config.toml
ENV RUST_LOG=info

USER rusttorrentd
VOLUME ["/data", "/var/lib/rusttorrentd", "/config"]
EXPOSE 8080 6881 6881/udp

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["rusttorrentd"]
