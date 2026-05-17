# On-demand Windows runner

The `Windows Smoke` workflow runs on the private `snapetech/packer` Windows VM
runner:

```yaml
runs-on: [self-hosted, Windows, X64, packer-windows]
```

The dispatcher starts a disposable VM overlay only when a queued job asks for
`packer-windows`. The runner is ephemeral and shuts down after one job.
