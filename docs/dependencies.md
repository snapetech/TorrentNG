# Dependency Policy

All workspace dependencies are pinned in `[workspace.dependencies]` in the root `Cargo.toml`.

Adding a dependency requires:
1. A clear statement of what it replaces or enables
2. License verification (must be MIT, Apache-2.0, or compatible)
3. Addition to this file with rationale

## Current dependencies

| Crate | Version | Rationale |
|-------|---------|-----------|
| tokio | 1 | async runtime |
| axum | 0.7 | HTTP/WebSocket server |
| serde / serde_json | 1 | serialization |
| toml | 0.8 | config file parsing |
| anyhow | 1 | error handling in binaries |
| thiserror | 1 | typed errors in libraries |
| tracing / tracing-subscriber | 0.1 / 0.3 | structured logging |
| rusqlite | 0.31 (bundled) | SQLite, no system dep |
| sha1, sha2 | 0.10 | piece hashing |
| reqwest | 0.12 | HTTP tracker announces |
| prometheus | 0.13 | metrics |
| bytes | 1 | byte buffer primitives |
| uuid | 1 | job and session IDs |
| rand | 0.8 | announce jitter, peer ID |
| url | 2 | tracker URL parsing |
| hex | 0.4 | infohash display |
| base64 | 0.22 | cookie auth |
| proptest | 1 | property tests |
