# Threat Model

## Assets

- Payload files and storage roots.
- TorrentNG-client session DB, torrent metadata, fastresume state, API tokens,
  and WebUI login credentials.
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
- Power loss or a crash leaving preallocated files partly zero-filled while
  resume state claims they are complete, handing corrupt media to downstream
  automation.

## Controls

- Torrent paths are normalized through safe relative path parsing.
- URL torrent add rejects private/local hosts.
- Bulk import/move/delete has dry-run and explicit apply paths.
- Tracker scheduling uses jitter and durable state.
- Public TorrentNG endpoints require configured API tokens. Native and
  compatible-client loopback WebUI browser requests require the configured
  username/password session; unauthenticated machine API calls remain
  available only on loopback.
- Unset passwords are generated randomly per installation and stored outside
  source-controlled config with owner-only permissions. Public binds require an
  API token and a WebUI password of at least 16 bytes; settings reads never
  return passwords.
- API tokens remain valid for machine clients and may be submitted in either
  WebUI login field.
- Script execution requires opt-in and explicit allowlisted directories.
- SQLite state is backed up before migration and import.
- Resume state is written only after payload data is synced; completion is held
  until data is durable; host crashes are detected and trigger bounded
  re-verification; migration refuses or downgrades trust for crashed sources
  ([CRASH_SAFETY.md](CRASH_SAFETY.md)).

## Residual Risk

Public BitTorrent traffic remains adversarial. Keep parser tests, fuzz targets,
and dependency scans in release gates. Do not expose unauthenticated mutating
APIs to the internet.
