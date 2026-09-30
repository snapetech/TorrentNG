---
category: fixed
audience: operators
area: container
action: none
breaking: false
---
Compatible-client images now assign the cache database in `/var/lib/torrentng` to the selected PUID/PGID before opening it, so Unraid and Compose volumes created by older root-running images start without a manual ownership change.
