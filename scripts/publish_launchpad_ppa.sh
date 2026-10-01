#!/usr/bin/env bash
set -euo pipefail

readonly REPO="snapetech/TorrentNG"
readonly WORKFLOW="release-ppa.yml"
readonly EXPECTED_FINGERPRINT="07E2531E1F470F8008ACFC996A07606FE65392F"
readonly ACTIONS_URL="https://github.com/snapetech/TorrentNG/actions/workflows/release-ppa.yml"
readonly PPA_PACKAGES_URL="https://launchpad.net/~keefshape/+archive/ubuntu/torrentng/+packages"

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Set the Launchpad signing secrets in GitHub, dispatch the PPA publisher, and
wait for its source uploads to finish.

Usage:
  scripts/publish_launchpad_ppa.sh [RELEASE_TAG]

With no tag, the latest published, non-prerelease GitHub release is selected.
The matching Launchpad upload private key must be available in the local GPG
keyring. The passphrase is read without echo and is sent to GitHub through stdin.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if (( $# > 1 )); then
  usage >&2
  exit 2
fi

for command_name in gh gpg gpgconf awk sed mktemp rm sleep; do
  command -v "$command_name" >/dev/null 2>&1 || fail "Required command not found: $command_name"
done

[[ -r /dev/tty ]] || fail "Run this script from an interactive terminal so it can securely prompt for the key passphrase."

gh auth status --hostname github.com >/dev/null 2>&1 ||
  fail "Authenticate gh to github.com with access to repository secrets and workflow dispatch."

actual_repo="$(gh repo view "$REPO" --json nameWithOwner --jq .nameWithOwner)" ||
  fail "Cannot access GitHub repository $REPO."
[[ "${actual_repo,,}" == "${REPO,,}" ]] || fail "GitHub resolved the unexpected repository: $actual_repo"

# Check secret-write access before prompting for the passphrase or exporting a key.
gh secret list --repo "$REPO" --app actions --json name >/dev/null ||
  fail "gh cannot manage Actions secrets in $REPO. Grant the authenticated account repository admin access."

if (( $# == 1 )); then
  release_tag="$1"
else
  release_tag="$(gh release list --repo "$REPO" \
    --exclude-drafts --exclude-pre-releases --limit 1 \
    --json tagName --jq '.[0].tagName // empty')" ||
    fail "Could not find the latest published stable release in $REPO."
fi

[[ -n "$release_tag" ]] || fail "No published stable release exists in $REPO."
if [[ ! "$release_tag" =~ ^main-[A-Za-z0-9][A-Za-z0-9._-]*$ ]] ||
  [[ "$release_tag" == *..* ]]; then
  fail "Invalid TorrentNG release tag: $release_tag"
fi

release_info="$(gh release view "$release_tag" --repo "$REPO" \
  --json tagName,isDraft,isPrerelease \
  --jq '[.tagName, (.isDraft | tostring), (.isPrerelease | tostring)] | @tsv')" ||
  fail "GitHub does not have a published release for tag $release_tag."
IFS=$'\t' read -r resolved_tag is_draft is_prerelease <<< "$release_info"
[[ "$resolved_tag" == "$release_tag" && "$is_draft" == false && "$is_prerelease" == false ]] ||
  fail "Tag $release_tag must identify a published, non-prerelease GitHub release."

secret_listing="$(gpg --batch --with-colons --fingerprint --list-secret-keys \
  "$EXPECTED_FINGERPRINT" 2>/dev/null)" ||
  fail "Could not read the Launchpad signing key from the local GPG keyring."
actual_fingerprint="$(awk -F: '$1 == "fpr" { print $10; exit }' <<< "$secret_listing")"
[[ "$actual_fingerprint" == "$EXPECTED_FINGERPRINT" ]] ||
  fail "The local GPG keyring must contain Launchpad's upload key $EXPECTED_FINGERPRINT."

key_header="$(gpg --batch --armor --export-secret-keys "$EXPECTED_FINGERPRINT" | sed -n '1p')" ||
  fail "Could not export the Launchpad private key from GPG."
[[ "$key_header" == "-----BEGIN PGP PRIVATE KEY BLOCK-----" ]] ||
  fail "GPG did not export a private key block. Import the matching exportable key and try again."

if ! IFS= read -r -s -p "Launchpad GPG passphrase (leave blank only for an unprotected key): " passphrase </dev/tty; then
  fail "Could not read the GPG passphrase."
fi
printf '\n' >/dev/tty
if ! IFS= read -r -s -p "Confirm GPG passphrase: " passphrase_confirm </dev/tty; then
  unset passphrase
  fail "Could not confirm the GPG passphrase."
fi
printf '\n' >/dev/tty
[[ "$passphrase" == "$passphrase_confirm" ]] || {
  unset passphrase passphrase_confirm
  fail "The passphrases did not match. No GitHub secrets were changed."
}
unset passphrase_confirm

# Import the export into a private temporary keyring and sign a short in-memory
# message. This catches hardware-only keys before changing GitHub secrets.
validation_gnupghome="$(mktemp -d)"
chmod 0700 "$validation_gnupghome"
cleanup() {
  unset passphrase
  if [[ -n "${validation_gnupghome:-}" && -d "$validation_gnupghome" ]]; then
    GNUPGHOME="$validation_gnupghome" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
    rm -rf -- "$validation_gnupghome"
  fi
}
trap cleanup EXIT

if ! gpg --batch --armor --export-secret-keys "$EXPECTED_FINGERPRINT" |
  GNUPGHOME="$validation_gnupghome" gpg --batch --import >/dev/null 2>&1; then
  fail "Could not import the private key into a temporary GPG keyring. No GitHub secrets were changed."
fi
exported_fingerprint="$(GNUPGHOME="$validation_gnupghome" gpg --batch --with-colons \
  --fingerprint --list-secret-keys "$EXPECTED_FINGERPRINT" 2>/dev/null |
  awk -F: '$1 == "fpr" { print $10; exit }')"
[[ "$exported_fingerprint" == "$EXPECTED_FINGERPRINT" ]] ||
  fail "The exported key does not match Launchpad's upload key. No GitHub secrets were changed."

if [[ -n "$passphrase" ]]; then
  if ! printf 'TorrentNG Launchpad key check\n' |
    GNUPGHOME="$validation_gnupghome" gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3 \
      --local-user "$EXPECTED_FINGERPRINT" --detach-sign --output /dev/null \
      3< <(printf '%s\n' "$passphrase"); then
    fail "The key could not sign locally with that passphrase. No GitHub secrets were changed."
  fi
else
  if ! printf 'TorrentNG Launchpad key check\n' |
    GNUPGHOME="$validation_gnupghome" gpg --batch --yes --pinentry-mode loopback \
      --local-user "$EXPECTED_FINGERPRINT" --detach-sign --output /dev/null; then
    fail "The key could not sign without a passphrase. Enter its passphrase and try again."
  fi
fi
GNUPGHOME="$validation_gnupghome" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
rm -rf -- "$validation_gnupghome"
validation_gnupghome=""

previous_run_id="$(gh run list --repo "$REPO" --workflow "$WORKFLOW" \
  --event workflow_dispatch --branch main --limit 1 \
  --json databaseId --jq '.[0].databaseId // 0')" ||
  fail "Could not check existing Launchpad workflow runs."

printf 'Saving the Launchpad upload key to GitHub Actions secrets for %s...\n' "$REPO"
if ! gpg --batch --armor --export-secret-keys "$EXPECTED_FINGERPRINT" |
  gh secret set LAUNCHPAD_GPG_PRIVATE_KEY --app actions --repo "$REPO"; then
  fail "GitHub rejected LAUNCHPAD_GPG_PRIVATE_KEY. The PPA workflow was not dispatched."
fi

if [[ -n "$passphrase" ]]; then
  if ! printf '%s' "$passphrase" |
    gh secret set LAUNCHPAD_GPG_PASSPHRASE --app actions --repo "$REPO"; then
    fail "GitHub rejected LAUNCHPAD_GPG_PASSPHRASE. The PPA workflow was not dispatched."
  fi
else
  passphrase_secret_exists="$(gh secret list --repo "$REPO" --app actions \
    --json name --jq 'any(.[]; .name == "LAUNCHPAD_GPG_PASSPHRASE")')" ||
    fail "Could not check for an existing passphrase secret."
  if [[ "$passphrase_secret_exists" == true ]]; then
    gh secret delete LAUNCHPAD_GPG_PASSPHRASE --app actions --repo "$REPO" ||
      fail "Could not remove the old passphrase secret for this unprotected key."
  fi
fi
unset passphrase
trap - EXIT

printf 'Dispatching %s for release %s...\n' "$WORKFLOW" "$release_tag"
gh workflow run "$WORKFLOW" --repo "$REPO" --ref main --field "tag=$release_tag" ||
  fail "GitHub could not dispatch the PPA workflow. Check $ACTIONS_URL"

run_id=""
for attempt in {1..60}; do
  observed_run_id="$(gh run list --repo "$REPO" --workflow "$WORKFLOW" \
    --event workflow_dispatch --branch main --limit 1 \
    --json databaseId --jq '.[0].databaseId // 0')" ||
    fail "Could not find the dispatched PPA workflow run. Check $ACTIONS_URL"
  if [[ "$observed_run_id" != 0 && "$observed_run_id" != "$previous_run_id" ]]; then
    run_id="$observed_run_id"
    break
  fi
  if (( attempt % 10 == 0 )); then
    printf 'Waiting for GitHub to create the workflow run... (%s/60)\n' "$attempt"
  fi
  sleep 2
done

if [[ -z "$run_id" ]]; then
  printf 'The dispatch was sent, but its run did not appear in the first 120 seconds.\n' >&2
  printf 'Check %s before retrying, to avoid submitting the release twice.\n' "$ACTIONS_URL" >&2
  exit 1
fi

run_url="$(gh run view "$run_id" --repo "$REPO" --json url --jq .url)"
printf 'Watching PPA upload run: %s\n' "$run_url"
if ! gh run watch "$run_id" --repo "$REPO" --compact --exit-status; then
  printf 'PPA upload workflow failed. Review %s\n' "$run_url" >&2
  exit 1
fi

printf '\nSigned source uploads were accepted by Launchpad for release %s.\n' "$release_tag"
printf 'Launchpad will build the installable packages asynchronously. Track them at:\n%s\n' "$PPA_PACKAGES_URL"
