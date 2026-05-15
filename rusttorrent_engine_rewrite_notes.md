Created the archive-ready implementation spec here:

[Download the RustTorrent engine rewrite spec](sandbox:/mnt/data/rusttorrent_engine_rewrite_spec.md)

Core framing: **do not ask a weaker/free model to “rewrite rTorrent.”** Ask it to implement one isolated crate, parser, endpoint, job, DB migration, or test at a time, with hard acceptance criteria.

The spec is built around these grounded constraints:

* **BEP 3 is the first protocol anchor**: bencode, exact `info` dictionary hashing, tracker announces, peer handshakes, request validation, choke/unchoke semantics, and piece serving have to be implemented strictly, not guessed. ([bittorrent.org][1])
* **Private tracker behavior must be first-class**: BEP 27 requires private torrents to announce only to the private tracker and only initiate connections to peers returned by that tracker; this directly shapes DHT/PEX/LSD defaults. ([bittorrent.org][2])
* **qBittorrent API compatibility is strategic**, because its WebUI API defines the ecosystem shape: `/api/v2/...`, cookie auth, `sync/maindata`, torrent list/properties/files/trackers, categories, tags, pause/resume/delete/recheck/reannounce, etc. ([GitHub][3])
* **Transmission is the RPC design reference**, especially its JSON-RPC 2.0 direction, structured request/response model, and compatibility-minded protocol evolution. ([GitHub][4])
* **Do not use mmap as the storage strategy** for this engine. The libtorrent discussion explicitly calls out that if you need substantive I/O control, you need to manage I/O in userspace rather than relying on the OS/mmap behavior. That matters directly for 200+ TB and multi-mount scheduling. ([GitHub][5])
* **rqbit proves Rust BitTorrent is viable**, with HTTP API, Web UI, library mode, DHT, PEX, UDP tracker, private torrent support, uTP, fastresume, rate limiting, and Prometheus metrics. But rqbit’s own open issues around pre-downloaded seeding/migration and QoS/per-disk scheduling show exactly why our rewrite must treat import, verification, and storage scheduling as core architecture, not later polish. ([GitHub][6])

The spec includes:

* workspace/crate layout
* implementation rules for weak/free agents
* crate-by-crate contracts
* storage scheduler design
* fastresume/session DB model
* tracker manager
* peer wire protocol
* seeding-first peer manager
* qBit compatibility layer
* Transmission compatibility layer
* SDK/CLI design
* fuzz/property/integration/scale tests
* benchmark suite
* security model
* phase plan
* first 25 implementation tickets
* reusable agent prompts

The most important architectural choice in the document is this:

> First useful release = **private-tracker-safe, seeding-first Rust daemon** that imports existing torrents, verifies files safely, announces correctly, serves pieces correctly, persists state, exposes qBit-compatible API, and remains responsive with 15k torrents.

That keeps the project implementable without letting a weak model wander into “build a whole torrent client” chaos.

[1]: https://www.bittorrent.org/beps/bep_0003.html "bep_0003.rst_post"
[2]: https://www.bittorrent.org/beps/bep_0027.html "bep_0027.rst_post"
[3]: https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-%28qBittorrent-5.0%29 "WebUI API (qBittorrent 5.0) · qbittorrent/qBittorrent Wiki · GitHub"
[4]: https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md "transmission/docs/rpc-spec.md at main · transmission/transmission · GitHub"
[5]: https://github.com/arvidn/libtorrent/issues/7551 "Why libtorrent 2.0's use of memory mapped files was a bad idea · Issue #7551 · arvidn/libtorrent · GitHub"
[6]: https://github.com/ikatson/rqbit/blob/main/README.md "rqbit/README.md at main · ikatson/rqbit · GitHub"

