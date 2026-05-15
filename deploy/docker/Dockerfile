# Stage 1: Build Rust sidecar
FROM rust:1.78-alpine AS sidecar-build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY sidecar/Cargo.toml sidecar/Cargo.lock ./
# Cache deps layer: build a stub main so cargo can fetch and compile deps
RUN mkdir src && echo 'fn main(){}' > src/main.rs && \
    cargo build --release && rm -rf src
COPY sidecar/src ./src
# Touch main.rs so cargo re-links the real binary
RUN touch src/main.rs && cargo build --release

# Stage 2: Build WebUI
FROM node:22-alpine AS webui-build
WORKDIR /src
COPY webui/package.json webui/package-lock.json ./
RUN npm ci --ignore-scripts
COPY webui/ ./
RUN npm run build

# Stage 3: Runtime
FROM alpine:3.20
RUN apk add --no-cache rtorrent ca-certificates tzdata

COPY --from=sidecar-build /src/target/release/rtorrentng /usr/local/bin/rtorrentng
COPY --from=webui-build   /src/dist /usr/share/rtorrentng/webui

COPY deploy/docker/rtorrent.rc    /etc/rtorrent/rtorrent.rc
COPY deploy/docker/entrypoint.sh  /entrypoint.sh
RUN chmod +x /entrypoint.sh

VOLUME ["/data", "/session", "/config"]
EXPOSE 8080

ENTRYPOINT ["/entrypoint.sh"]
