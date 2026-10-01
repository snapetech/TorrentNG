---
category: added
audience: users, operators
area: auth
action: none
breaking: false
---
Fresh loopback installs on both TorrentNG profiles can sign in with `torrentng` / `torrentng`; administrators can change the WebUI username and password in Settings -> Security or in `[auth]` in config.toml. API-token login remains supported on native and existing-client backends, and tokens work in either login field. Unraid templates point directly to their API Token setting and explain the first-login path. Public binds refuse the default password.
