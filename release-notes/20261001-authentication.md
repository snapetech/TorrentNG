---
category: security
audience: users, operators
area: authentication
action: Run `torrentngd auth-token` (or `torrentng auth-token` for the compatibility service) to retrieve credentials; replace any explicitly configured shared password in config or Security settings.
breaking: true
---
Fresh installs and upgrades without an explicit `auth.password` now use a unique, private WebUI password instead of a shared default. Configured API tokens work in either login field, and credentials remain configurable in Security settings.
