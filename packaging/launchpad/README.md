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

## First PPA publish

From an interactive terminal on a machine with `gh` authenticated to
`snapetech/TorrentNG` and the Launchpad upload private key in its local GPG
keyring, run:

```sh
scripts/publish_launchpad_ppa.sh [RELEASE_TAG]
```

With no tag, the script selects the latest published stable GitHub release. It
checks the local key fingerprint and signing access, prompts for the passphrase
without echoing it, writes `LAUNCHPAD_GPG_PRIVATE_KEY` and (when needed)
`LAUNCHPAD_GPG_PASSPHRASE` to GitHub Actions through stdin, dispatches
**Publish Launchpad PPA**, and waits for the source uploads to finish. For a
key without a passphrase, leave both hidden prompts blank; the script removes
any stale passphrase secret. Launchpad still builds the `.deb` packages after
the script finishes, so use the [PPA packages page](https://launchpad.net/~keefshape/+archive/ubuntu/torrentng/+packages)
to track build completion.

The PPA upload key is the OpenPGP key on the Launchpad account profile. It is
different from the PPA's repository-signing key, which Launchpad manages for
APT clients. The workflow imports the private key into an ephemeral runner
keyring, verifies its fingerprint, signs the source package, and uploads it
with `dput`. The script validates the export in a private temporary GPG
keyring, then streams the key from GPG to `gh`. It never writes the passphrase
to a file. Do not commit or paste either value into the repository.

Later releases dispatch this workflow automatically. The workflow fails before
uploading if the private key is missing or its fingerprint does not match.

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
