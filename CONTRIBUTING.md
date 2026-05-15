# Contributing

Contributions should preserve the project's security model: rTorrent SCGI is a
trusted local interface only, and all browser or automation access must go
through the sidecar.

By contributing, you agree that your contribution is provided under the project
license: `AGPL-3.0-or-later OR Commercial`.

Do not contribute code or assets that you do not have the right to license.
Do not add functionality whose primary purpose is piracy, copyright
infringement, or evading lawful access controls.

Before submitting changes, run the relevant checks:

```sh
cd sidecar && cargo test
cd webui && npm run build
```

