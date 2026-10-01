#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: build_launchpad_ppa_source.sh <release-tag> <ubuntu-series> <run-id> <run-attempt> <output-dir> [include-orig] [shared-orig]

Builds a signed Launchpad source package containing the matching amd64 and
arm64 TorrentNG release assets. Requires gh authentication, dpkg packaging
tools, and the Launchpad-authorized private OpenPGP key in GPG's keyring.
USAGE
}

if [[ $# -lt 5 || $# -gt 7 ]]; then
  usage
  exit 2
fi

release_tag="$1"
series="$2"
run_id="$3"
run_attempt="$4"
output_dir="$5"
include_orig="${6:-true}"
shared_orig="${7:-}"

if [[ "$include_orig" != true && "$include_orig" != false ]]; then
  echo "include-orig must be true or false." >&2
  exit 2
fi

if [[ ! "$release_tag" =~ ^main-[A-Za-z0-9][A-Za-z0-9._-]*$ ]] ||
  [[ "$release_tag" == *..* ]] || ! git check-ref-format "refs/tags/$release_tag"; then
  echo "Invalid release tag: $release_tag" >&2
  exit 1
fi

case "$series" in
  jammy) ubuntu_version=22.04 ;;
  noble) ubuntu_version=24.04 ;;
  resolute) ubuntu_version=26.04 ;;
  *) echo "Unsupported Ubuntu series: $series" >&2; exit 1 ;;
esac

if [[ ! "$run_id" =~ ^[0-9]+$ || ! "$run_attempt" =~ ^[0-9]+$ ]]; then
  echo "The workflow run id and attempt must be numeric." >&2
  exit 1
fi

expected_key="07E2531E1F470F8008ACCFC996A07606FE65392F"
signing_key="${LAUNCHPAD_GPG_FINGERPRINT:-$expected_key}"
if [[ "$signing_key" != "$expected_key" ]]; then
  echo "Launchpad signing key does not match the account key configured for this PPA." >&2
  exit 1
fi

repo_root="$(git rev-parse --show-toplevel)"
workspace="$(mktemp -d "${TMPDIR:-/tmp}/torrentng-ppa-${series}.XXXXXX")"
asset_dir="$workspace/assets"
mkdir -p "$asset_dir" "$output_dir/$series"

upstream_version="${release_tag#main-}"
upstream_version="${upstream_version#v}"
upstream_version="${upstream_version//-/.}"
if [[ ! "$upstream_version" =~ ^[0-9][A-Za-z0-9.+:~]*$ ]]; then
  echo "Release tag does not produce a valid Debian upstream version: $release_tag" >&2
  exit 1
fi

debian_version="${upstream_version}-0ubuntu1~ppa1~ubuntu${ubuntu_version}.1+gh${run_id}.r${run_attempt}"
source_root="$workspace/torrentngd-${upstream_version}"
mkdir -p "$source_root"

gh release download "$release_tag" \
  --repo snapetech/TorrentNG \
  --dir "$asset_dir" \
  --pattern "torrentngd-native-${release_tag}-linux-x86_64.tar.gz" \
  --pattern "torrentngd-native-${release_tag}-linux-aarch64.tar.gz" \
  --pattern "SHA256SUMS-${release_tag}.txt" >&2

(
  cd "$asset_dir"
  checksum_file="SHA256SUMS-${release_tag}.txt"
  for asset in \
    "torrentngd-native-${release_tag}-linux-x86_64.tar.gz" \
    "torrentngd-native-${release_tag}-linux-aarch64.tar.gz"; do
    grep -Fq "  $asset" "$checksum_file" || {
      echo "Release checksum file has no entry for $asset" >&2
      exit 1
    }
  done
  sha256sum --ignore-missing --check --status "$checksum_file"
)

source_epoch="$(git log -1 --format=%ct "$release_tag^{commit}")"
if [[ ! "$source_epoch" =~ ^[0-9]+$ ]]; then
  echo "Could not determine the source commit timestamp for $release_tag." >&2
  exit 1
fi
git archive --format=tar "$release_tag" | tar -xf - -C "$source_root"

mkdir -p "$source_root/release-assets/amd64" "$source_root/release-assets/arm64"
tar -xzf "$asset_dir/torrentngd-native-${release_tag}-linux-x86_64.tar.gz" \
  -C "$source_root/release-assets/amd64"
tar -xzf "$asset_dir/torrentngd-native-${release_tag}-linux-aarch64.tar.gz" \
  -C "$source_root/release-assets/arm64"

for architecture in amd64 arm64; do
  test -x "$source_root/release-assets/$architecture/usr/bin/torrentngd"
  test -s "$source_root/release-assets/$architecture/usr/share/torrentng/webui/index.html"
done

orig_basename="torrentngd_${upstream_version}.orig.tar.gz"
orig_tarball="$output_dir/$series/$orig_basename"
if [[ -n "$shared_orig" ]]; then
  [[ -s "$shared_orig" ]] || {
    echo "Shared upstream source archive does not exist: $shared_orig" >&2
    exit 1
  }
  cp -- "$shared_orig" "$orig_tarball"
elif [[ "$include_orig" == true ]]; then
  if [[ ! -f "$orig_tarball" ]]; then
    tar --sort=name --mtime="@${source_epoch}" --owner=0 --group=0 --numeric-owner \
      --format=gnu -cf - -C "$workspace" "torrentngd-${upstream_version}" | gzip -n > "$orig_tarball"
  fi
else
  orig_url="https://launchpad.net/~keefshape/+archive/ubuntu/torrentng/+files/$orig_basename"
  curl --fail --silent --show-error --location --retry 3 --max-time 120 \
    "$orig_url" --output "$orig_tarball" || {
    echo "Could not retrieve the existing Launchpad source archive: $orig_basename" >&2
    exit 1
  }
  gzip --test "$orig_tarball" || {
    rm -f -- "$orig_tarball"
    echo "Launchpad returned an invalid source archive: $orig_basename" >&2
    exit 1
  }
fi
cp -- "$orig_tarball" "$workspace/"

cp -a "$repo_root/packaging/launchpad/debian" "$source_root/debian"
source_date="$(date -Ru)"
cat > "$source_root/debian/changelog" <<CHANGELOG
torrentngd (${debian_version}) ${series}; urgency=medium

  * Package TorrentNG release ${release_tag} for Launchpad.

 -- TorrentNG Maintainers <keith@snape.tech>  ${source_date}
CHANGELOG

signer="$repo_root/scripts/launchpad-gpg-sign"
(
  cd "$source_root"
  source_archive_option=-sd
  if [[ "$include_orig" == true ]]; then
    source_archive_option=-sa
  fi
  dpkg-buildpackage -S "$source_archive_option" \
    -k"$signing_key" \
    -p"$signer" \
    --sign-backend=gpg
) >&2

changes_file="$workspace/torrentngd_${debian_version}_source.changes"
if [[ ! -f "$changes_file" ]]; then
  echo "dpkg-buildpackage did not create the expected source changes file." >&2
  exit 1
fi

cp "$workspace"/torrentngd_"$debian_version"* "$output_dir/$series/"
printf '%s\n' "$output_dir/$series/$(basename "$changes_file")"
