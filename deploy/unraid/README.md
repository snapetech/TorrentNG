# deploy/unraid/

Unraid Community Applications (CA) templates for TorrentNG. See
[deploy/README.md](../README.md) for the underlying Docker images and
[docs/DEPLOYMENT.md](../../docs/DEPLOYMENT.md) /
[docs/NATIVE_DEPLOYMENT.md](../../docs/NATIVE_DEPLOYMENT.md) for the
non-Unraid deployment paths these templates wrap.

| Path | What it is |
|---|---|
| `templates/torrentng-webui.xml` | WebUI/API replacement for an rTorrent, qBittorrent, Transmission, or Deluge install you already run. Image: `ghcr.io/snapetech/torrentng/sidecar`. |
| `templates/torrentng.xml` | Full first-party stack: `torrentngd` with its built-in WebUI/API, no external client. Image: `ghcr.io/snapetech/torrentng/native`. |
| `ca_profile.xml` | Repository profile shown by Community Applications (author/support info). Must live at the root of whatever repo is actually submitted to CA -- see "Getting listed in Community Applications" below. |
| `icon.svg`, `icon.png` | App icon referenced by both templates and `ca_profile.xml`. `icon.png` is the rasterized 256x256 version (`rsvg-convert -w 256 -h 256 icon.svg -o icon.png`); regenerate it if `icon.svg` changes. |

## Which template

- Already running rTorrent, qBittorrent, Transmission, or Deluge (on this
  box or elsewhere on the network) and just want a modern replacement WebUI
  and a qBittorrent-compatible API for Prowlarr/Sonarr/Radarr/autobrr/
  cross-seed? Use **torrentng-webui**.
- Want one box that does everything with no external client dependency? Use
  **torrentng** (runs `torrentngd`, TorrentNG's own first-party engine).

Both are documented in depth in their own `<Overview>` text, which Unraid
shows on the Add Container page.

## Try it without submitting anywhere (works today)

Unraid can install directly from a raw template XML URL, no CA submission
or review required:

1. Docker tab -> **Add Container**.
2. In the **Template** field at the top, paste the raw GitHub URL of the
   template you want, e.g.
   `https://raw.githubusercontent.com/snapetech/TorrentNG/main/deploy/unraid/templates/torrentng-webui.xml`.
3. The form populates from the template. Fill in the required fields
   (Secret Key, API Tokens, and the backend URL/credentials for
   `torrentng-webui`; the API Token and Config path for `torrentng`) and
   Apply.

This is enough for personal use or sharing a direct link with other
TorrentNG users before/without a formal CA listing.

## Getting listed in Community Applications

Full discoverability (searchable in the Apps tab for every Unraid user
without them having anything pasted in first) goes through
<https://ca.unraid.net/submit>, which live-scans a **repository root** for
`ca_profile.xml` plus one XML per app under `templates/`.

`ca_profile.xml` in this directory is written as if `deploy/unraid/` *is*
that repository root (matching the layout of
<https://github.com/unraid/unraid-community-apps-starter>, the official
starter template). Since TorrentNG is a monorepo, submitting requires one
of:

1. **Mirror `deploy/unraid/` into a dedicated repo** (e.g.
   `snapetech/torrentng-unraid-templates`), matching how most multi-app
   maintainers do this (binhex, ibracorp, etc. all keep templates in a repo
   separate from the application source). Recommended: keeps template
   release cadence independent of the main TorrentNG repo, and matches
   what CA moderators expect to see. After creating it, update every
   `raw.githubusercontent.com/snapetech/TorrentNG/main/deploy/unraid/...`
   URL in `ca_profile.xml` and both template XMLs to the new repo's raw
   URLs (including each template's own `<TemplateURL>` -- it must point at
   itself).
2. **Submit the TorrentNG repo directly**, moving `ca_profile.xml` to the
   repo root and updating the same URLs to drop the `deploy/unraid/`
   segment. Simpler, but puts CA-specific metadata at the top level of an
   otherwise unrelated software repo.

Either way, before submitting:

- Push the icon, `ca_profile.xml`, and both templates so the raw URLs
  referenced in the XML actually resolve (the CA scanner fetches them
  live).
- Run **Validate** then **Scan** at `/submit/new` and fix anything it
  flags.
- Confirm the repository has an OSI-approved license covering the
  submitted templates/metadata (TorrentNG's existing `LICENSE` applies if
  submitting the main repo; a mirrored repo needs its own).

## Design notes for future edits

- **Multi-backend template, no conditional fields.** Unraid templates
  can't show/hide `<Config>` entries based on another field's value, so
  `torrentng-webui.xml` lists every backend's variables with
  `Display="advanced"` and per-field descriptions saying which "Backend
  Type" selection they apply to, following the same pattern used by
  multi-provider templates like binhex's VPN-enabled containers.
- **`--user 99:100` in `<ExtraParams>` on both templates.** Neither
  `torrentngd` nor the compatible-client service has Unraid-style
  PUID/PGID env-var handling; both images bake a fixed non-root user at
  build time. Overriding to Unraid's standard `nobody:users` (99:100) at
  `docker run` time is the standard fix for non-LSIO images on Unraid --
  make sure the appdata/data folders are owned by 99:100 (Unraid's Tools ->
  New Permissions handles this).
- **`torrentngd` has no per-field env var overrides by design** (see
  `crates/rt-config`), unlike the compatible-client service. Its Unraid
  template (`torrentng.xml`) and `deploy/native/entrypoint.sh` bridge this
  with exactly two additions: default `TORRENTNGD_CONFIG` to a `/config`
  directory mount (falling back to the packaged default config if nothing
  is mounted there), and an optional `TORRENTNGD_API_TOKEN` env var that
  gets written to the path the packaged config already expects for
  `auth.api_tokens_file`. This does not change `torrentngd`'s documented
  file-based configuration convention -- power users can still mount a
  fully custom `config.toml` and set `TORRENTNGD_CONFIG` themselves exactly
  as `deploy/native/compose.yml` does.
- **Fronting an existing rTorrent needs `TNG_RTORRENT_MANAGED=0`.**
  Before this template existed, the compatible-client service's
  `entrypoint.sh` always launched and owned its own rTorrent process
  whenever `TNG_BACKEND=rtorrent` -- there was no way to point it at an
  rTorrent instance the operator already runs, unlike qBittorrent/
  Transmission/Deluge, which were always network-address-based. See the
  `TNG_RTORRENT_MANAGED` handling added to
  `deploy/docker/entrypoint.sh` (default `1`, matching every existing
  compose profile's behavior unchanged; the Unraid template sets it to
  `0`).
- **Tracker identity.** The compatible-client service pushes its
  User-Agent/peer_id to whatever rTorrent it connects to on every start
  (`docs/TRACKER-IDENTITY.md`). Both templates carry that warning verbatim
  in their `<Overview>`/field descriptions -- don't trim it out when
  editing; a shared/hardcoded peer_id previously got a real user banned
  from a private tracker.
