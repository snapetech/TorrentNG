---
category: security
audience: users, operators
area: authentication
action: Run `torrentngd auth-token` interactively; use `torrentng auth-token` for the compatibility service. Replace explicitly configured shared passwords.
breaking: true
---
Fresh installs and upgrades without an explicit `auth.password` now use a unique, private WebUI password instead of a shared default. Configured API tokens work in either login field, and credentials remain configurable in Security settings. The native `auth-token` command writes credentials only to the controlling terminal and refuses redirected or non-interactive output.
