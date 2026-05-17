# Stage 1: Build Rust sidecar
FROM rust:1.93-alpine AS sidecar-build
RUN apk add --no-cache musl-dev pkgconf openssl-dev openssl-libs-static
WORKDIR /src
COPY sidecar/Cargo.toml sidecar/Cargo.lock ./
# Cache deps layer: build a stub main so cargo can fetch and compile deps
RUN mkdir src && echo 'fn main(){}' > src/main.rs && echo '' > src/lib.rs && \
    cargo build --release && rm -rf src
COPY sidecar/src ./src
# Touch lib.rs and main.rs so cargo rebuilds the real crate after the dependency cache layer.
RUN touch src/lib.rs src/main.rs && cargo build --release

# Stage 2: Build WebUI
FROM node:22-bookworm-slim AS webui-build
WORKDIR /src
COPY webui/package.json webui/package-lock.json ./
RUN npm ci --ignore-scripts
COPY webui/ ./
RUN npm run build

# Stage 3: Build modern rTorrent/libtorrent.
# The patch exposes libtorrent's HTTP user-agent getter/setter through rTorrent XMLRPC.
FROM alpine:3.20 AS rtorrent-build
ARG RTORRENT_VERSION=0.16.11
ARG LIBTORRENT_VERSION=0.16.11
RUN apk add --no-cache \
    autoconf automake build-base cmake curl-dev git libtool linux-headers \
    ncurses-dev ninja openssl-dev pkgconf tinyxml2-dev zlib-dev
WORKDIR /src
RUN git clone --depth=1 --branch "v${LIBTORRENT_VERSION}" https://github.com/rakshasa/libtorrent.git && \
    git clone --depth=1 --branch "v${RTORRENT_VERSION}" https://github.com/rakshasa/rtorrent.git
COPY deploy/docker/patches/rtorrent-0.16.11-user-agent-command.patch /tmp/rtorrent-user-agent-command.patch
COPY deploy/docker/patches/rtorrent-0.16.11-multicall-range.patch /tmp/rtorrent-multicall-range.patch
RUN cd rtorrent && git apply /tmp/rtorrent-user-agent-command.patch && git apply /tmp/rtorrent-multicall-range.patch
RUN cd libtorrent && autoreconf -fi && \
    ./configure --prefix=/usr/local && \
    make -j"$(nproc)" && make install
RUN cd rtorrent && autoreconf -fi && \
    ./configure --prefix=/usr/local --with-xmlrpc-tinyxml2 && \
    make -j"$(nproc)" && make install

# Stage 4: Runtime
FROM alpine:3.20
RUN apk add --no-cache ca-certificates curl libcurl ncurses openssl tinyxml2 tzdata zlib

COPY --from=sidecar-build /src/target/release/torrentng /usr/local/bin/torrentng
COPY --from=webui-build   /sidecar/static /usr/share/torrentng/webui
COPY --from=rtorrent-build /usr/local/bin/rtorrent /usr/local/bin/rtorrent
COPY --from=rtorrent-build /usr/local/lib/libtorrent* /usr/local/lib/

COPY deploy/docker/rtorrent.rc    /etc/rtorrent/rtorrent.rc
COPY engine-profile/rtorrent.rc   /etc/rtorrent/profile.rc
COPY deploy/docker/sidecar.config.toml /etc/torrentng/config.toml
COPY deploy/docker/entrypoint.sh  /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV TNG_STATIC_DIR=/usr/share/torrentng/webui
ENV LD_LIBRARY_PATH=/usr/local/lib
ENV TNG_PACKAGED_RTORRENT_VERSION=0.16.11
ENV TNG_PACKAGED_LIBTORRENT_VERSION=0.16.11
ENV TNG_RTORRENT_PATCHES=rtorrent-0.16.11-user-agent-command,rtorrent-0.16.11-multicall-range

VOLUME ["/data", "/session", "/config"]
EXPOSE 8080 50000 50000/udp

ENTRYPOINT ["/entrypoint.sh"]
