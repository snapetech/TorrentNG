# WebUI torrent-list audit

This audit compares the TorrentNG torrent workspace with the fields operators
expect from qBittorrent, Transmission, and ruTorrent-style tables. The live
deployment was checked through its served HTML/JavaScript and the sidecar API
model was checked against the source and compatibility projections.

## Findings and fixes

| Area | Finding | Result |
| --- | --- | --- |
| Header alignment | The header reserved `96px` for its controls while rows reserved only `12px`. The flexible name track absorbed the difference, so columns after Name drifted left. | Header and rows now use the same grid tracks and padding. A required action track holds the controls without changing data-column geometry. |
| Queue state | An open, started torrent with no current transfer rate was rendered as `Queued`. | Open-but-idle torrents render as `Stalled`; only closed, ready torrents render as `Queued`. The sidecar filters use the same distinction. |
| Progress context | The table showed size and percentage but not the amount remaining or a time estimate. | Added `Left` and `ETA`. ETA is derived from remaining bytes and current download rate; active torrents without a rate show `∞` instead of a fabricated duration. |
| Swarm context | Seeds and connected peers were available in the model but absent from the table. | Added separate `Seeds` and `Peers` columns. |
| Lifecycle context | Added/completed timestamps were incomplete in the default view. | Added `Completed`; `Added` remains sortable. |
| Transfer accounting | Total downloaded/uploaded bytes were only available in details/API views. | Added optional `Downloaded` and `Uploaded` columns. |
| Operational metadata | Priority, tracker, category, tags, and save path were not all available as table columns. | Added optional `Priority` and `Save path`; existing category, tags, and tracker columns remain available. |

## Default and optional columns

The default view now contains the high-signal fields for triage:

`Type`, `Name`, `Status`, `Size`, `%`, `Left`, `ETA`, download/upload rate,
`Seeds`, `Peers`, `Ratio`, `Added`, `Completed`, `Category`, `Tags`, and
`Tracker`.

The column menu also exposes `Downloaded`, `Uploaded`, `Priority`, and `Save
path`. The compact preset keeps the transfer/status fields while retaining the
ETA and swarm context.

## Remaining compatibility gaps

The list model does not yet carry every field exposed by the comparison clients.
These should be added as explicit API fields before being displayed rather than
inferred in the browser:

- swarm totals (`num_complete`/`num_incomplete`) and availability;
- last activity, average transfer rates, and per-torrent rate limits;
- private-torrent/force-start flags and seed/time ratio limits;
- tracker tier/status/message and next reannounce time;
- piece availability and file-level completion summaries.

Those fields are already represented by qBittorrent's torrent-info contract and
Transmission's `torrent-get` vocabulary, but the current TorrentNG summary
projection does not expose them consistently across engines. The detail and
tracker views remain the right place for the information until the projection
is extended.

Reference contracts:

- [qBittorrent WebUI API torrent fields](https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29)
- [Transmission RPC specification](https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md)
