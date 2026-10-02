---
category: security
audience: users, operators
area: authentication
action: Run `torrentng auth-token` from an interactive terminal.
breaking: true
---
The compatible-client `auth-token` command now writes credentials only to the controlling terminal. It fails without an interactive terminal, so stdout redirection and pipelines cannot capture credentials.
