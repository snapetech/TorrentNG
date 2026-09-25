---
category: fixed
audience: operators
area: deployment
action: Use the authenticated reverse proxy for remote WebUI/API access; the systemd service now binds its HTTP endpoint to loopback.
breaking: true
---
Systemd compatible-client deployments now prepare mounted cache databases for the non-root runtime, so root-owned legacy data no longer prevents startup. The service binds its WebUI/API to loopback; remote access must go through an authenticated reverse proxy.
