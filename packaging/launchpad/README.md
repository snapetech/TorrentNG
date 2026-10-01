# Launchpad PPA publishing

The public Ubuntu PPA is
[`ppa:keefshape/torrentng`](https://launchpad.net/~keefshape/+archive/ubuntu/torrentng).
`release.yml` dispatches `release-ppa.yml` for each published TorrentNG
release. That workflow uploads signed source packages for Jammy, Noble, and
Resolute; Launchpad builds `.deb` packages for amd64 and arm64.

The PPA is empty until the signing secrets are added and the first publish
workflow succeeds. The install commands below become usable after that upload.

The source package contains the exact, checksummed Linux release assets for
both architectures plus the corresponding upstream source tree. Launchpad
builds the Debian package wrapper for each architecture; it does not rebuild
the Rust daemon. This reuses the binaries already built and checked by the
release workflow. The release job verifies the asset checksums before creating
any upload.

## Required GitHub Actions secrets

The repository currently has no Launchpad signing secrets. Add these secrets
to `snapetech/TorrentNG` before dispatching the PPA workflow:

- `LAUNCHPAD_GPG_PRIVATE_KEY`: ASCII-armored private key associated with the
  Launchpad upload key `07E2531E1F470F8008ACFC996A07606FE65392F`.
- `LAUNCHPAD_GPG_PASSPHRASE`: passphrase for that key.

The PPA upload key is the OpenPGP key on the Launchpad account profile. It is
different from the PPA's repository-signing key, which Launchpad manages for
APT clients. The workflow imports the private key into an ephemeral runner
keyring, verifies its fingerprint, signs the source package, and uploads it
with `dput`. Do not commit or paste the private key or passphrase into the
repository.

Once the secrets are configured, dispatch **Publish Launchpad PPA** with an
already-published tag to publish that release. Later releases dispatch this
workflow automatically. The workflow fails before uploading if the secrets are
missing or the key fingerprint does not match.

## User installation

```sh
sudo add-apt-repository ppa:keefshape/torrentng
sudo apt update
sudo apt install torrentngd
```

The package supports `jammy`, `noble`, and `resolute`, on `amd64` and `arm64`.
Its systemd service binds the API to loopback by default; see
[`NATIVE_DEPLOYMENT.md`](../../docs/NATIVE_DEPLOYMENT.md#ubuntu-ppa-packages)
before exposing the service through a non-loopback bind or reverse proxy.
