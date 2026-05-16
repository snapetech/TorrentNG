# Agent Implementation Rules

These rules apply to every implementation task in this workspace.

## Global Rules

1. Do not invent protocol behavior. Read the BEP or local spec file first.
2. Do not add dependencies without updating `docs/dependencies.md`.
3. Do not use unsafe Rust unless the crate-level README contains a written justification and tests.
4. Do not use mmap for torrent payload data in v1.
5. Do not expose any mutating API without authentication.
6. Do not implement delete/move operations without dry-run mode first.
7. Do not implement a "torrent complete" transition unless all pieces are verified.
8. Do not perform path joins from torrent metadata without passing through `rt-path`.
9. Do not make qBittorrent compatibility structs the internal engine model.
10. Do not use a global mutex around session state.
11. Do not use unbounded queues for peer piece data.
12. Do not make recheck an inline API request. It must be a job.
13. Do not silently skip corrupt metadata; return typed errors.
14. Do not rely on wall-clock time for deterministic tests; inject clock.
15. Do not parse bencode with generic JSON/YAML/TOML libraries.

## Per-PR Checklist

Every implementation PR must include:

- [ ] unit tests
- [ ] at least one negative test
- [ ] tracing events where operationally relevant
- [ ] typed error variants
- [ ] no `unwrap()`/`expect()` in library code except tests
- [ ] no blocking I/O in async tasks unless isolated and documented
- [ ] README update for public crate APIs
- [ ] acceptance criteria copied into PR body
