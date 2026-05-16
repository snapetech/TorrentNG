# Threat Model

## Assets

- Payload files and storage roots.
- Native session DB, torrent metadata, fastresume state, and API tokens.
- Automation integrations such as Sonarr, Radarr, Prowlarr, autobrr, and
  cross-seed.

## Trust Boundaries

- Public tracker and peer traffic is untrusted.
- Torrent files, magnet links, tracker responses, and peer messages are
  untrusted parser inputs.
- Compatibility APIs are trusted only after API/session authentication.
- Script workflows are privileged local execution and must remain disabled by
  default.

## Main Risks

- Malicious torrent metadata attempting path traversal or resource exhaustion.
- SSRF through URL-based torrent adds.
- Destructive bulk operations against the wrong storage root.
- Tracker announce storms after restart.
- Token leakage through logs, reverse proxies, or browser storage.
- Script workflow escape if an operator enables broad script directories.

## Controls

- Torrent paths are normalized through safe relative path parsing.
- URL torrent add rejects private/local hosts.
- Bulk import/move/delete has dry-run and explicit apply paths.
- Tracker scheduling uses jitter and durable state.
- Mutating native endpoints require configured API tokens.
- Script execution requires opt-in and explicit allowlisted directories.
- SQLite state is backed up before migration and import.

## Residual Risk

Public BitTorrent traffic remains adversarial. Keep parser tests, fuzz targets,
and dependency scans in release gates. Do not expose unauthenticated mutating
APIs to the internet.
