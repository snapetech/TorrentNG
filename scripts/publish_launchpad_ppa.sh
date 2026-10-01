#!/usr/bin/env bash
set -euo pipefail

readonly REPO="snapetech/TorrentNG"
readonly WORKFLOW="release-ppa.yml"
readonly EXPECTED_FINGERPRINT="07E2531E1F470F8008ACCFC996A07606FE65392F"
readonly ADDITIONAL_KEY_UID="slskdn (TorrentNG additional Launchpad key) <slskdn@proton.me>"
readonly ADDITIONAL_KEY_SECRET_SERVICE="torrentng-launchpad-ppa"
readonly LAUNCHPAD_KEY_PAGE="https://launchpad.net/~keefshape/+editpgpkeys"
readonly LAUNCHPAD_KEYS_API="https://api.launchpad.net/1.0/~keefshape/gpg_keys"
readonly UBUNTU_KEYSERVER="hkps://keyserver.ubuntu.com"
readonly ACTIONS_URL="https://github.com/snapetech/TorrentNG/actions/workflows/release-ppa.yml"
readonly PPA_PACKAGES_URL="https://launchpad.net/~keefshape/+archive/ubuntu/torrentng/+packages"

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Set the Launchpad signing secrets in GitHub, dispatch the PPA publisher, and
wait for its source uploads to finish. It also creates and enrolls an
additional Launchpad signing key, preserving the existing release key.

Usage:
  scripts/publish_launchpad_ppa.sh [RELEASE_TAG]

With no tag, the latest published, non-prerelease GitHub release is selected.
The existing Launchpad upload key must be available in the local GPG keyring.
The new key's passphrase is kept in Secret Service. The script pauses for the
Launchpad import and confirmation steps, then reads the existing key's
passphrase without echo and sends it to GitHub through stdin.
EOF
}

additional_key_fingerprint() {
  gpg --batch --with-colons --fingerprint --list-secret-keys 2>/dev/null |
    awk -F: -v wanted="$ADDITIONAL_KEY_UID" '
      $1 == "sec" { primary=1; fingerprint=""; next }
      $1 == "ssb" { primary=0; next }
      $1 == "fpr" && primary { fingerprint=$10; primary=0; next }
      $1 == "uid" && $10 == wanted && fingerprint != "" { print fingerprint }
    '
}

launchpad_has_key() {
  python3 - "$1" "$LAUNCHPAD_KEYS_API" <<'PY'
import json
import sys
import urllib.error
import urllib.request

fingerprint, url = sys.argv[1:]
try:
    with urllib.request.urlopen(url, timeout=20) as response:
        payload = json.load(response)
except (OSError, urllib.error.URLError, json.JSONDecodeError) as error:
    print(f"Could not check Launchpad key registration: {error}", file=sys.stderr)
    sys.exit(2)

if any(entry.get("fingerprint", "").replace(" ", "").upper() == fingerprint
       for entry in payload.get("entries", []) if isinstance(entry, dict)):
    sys.exit(0)
sys.exit(1)
PY
}

keyserver_has_key() {
  local fingerprint="$1"
  local url="https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x${fingerprint}"

  curl --fail --silent --show-error --location --max-time 20 "$url" 2>/dev/null |
    gpg --batch --with-colons --import-options show-only --dry-run --import 2>/dev/null |
    awk -F: '$1 == "fpr" { print $10; exit }'
}

ensure_additional_key() {
  local fingerprint passphrase pending_passphrase observed_fingerprint
  local attempt registered
  local challenge="" message_line signed_challenge

  fingerprint="$(additional_key_fingerprint)"
  if [[ -z "$fingerprint" ]]; then
    if pending_passphrase="$(secret-tool lookup \
      service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint pending 2>/dev/null)"; then
      passphrase="$pending_passphrase"
    else
      passphrase="$(openssl rand -base64 36 | tr -d '\n')"
      printf '%s' "$passphrase" | secret-tool store \
        --label='TorrentNG additional Launchpad signing key' \
        service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint pending ||
        fail "Could not store the new key passphrase in Secret Service."
    fi
    unset pending_passphrase

    printf 'Creating an additional signing key; the current release key stays unchanged...\n'
    if ! printf '%s\n' "$passphrase" |
      gpg --batch --pinentry-mode loopback --passphrase-fd 0 \
        --quick-generate-key "$ADDITIONAL_KEY_UID" rsa4096 sign 3y; then
      unset passphrase
      fail "GPG could not create the additional signing key. The passphrase remains in Secret Service under the pending entry."
    fi
    fingerprint="$(additional_key_fingerprint)"
    [[ "$fingerprint" =~ ^[A-F0-9]{40}$ ]] ||
      fail "Could not identify the new signing key by its user ID."
    printf '%s' "$passphrase" | secret-tool store \
      --label='TorrentNG additional Launchpad signing key' \
      service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint "$fingerprint" ||
      fail "Could not associate the new key passphrase with its fingerprint."
    secret-tool clear service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint pending >/dev/null ||
      fail "The new key exists, but its temporary Secret Service entry could not be cleared."
    unset passphrase
  fi

  [[ "$fingerprint" =~ ^[A-F0-9]{40}$ ]] ||
    fail "The additional key UID resolved to an invalid fingerprint."
  if ! secret-tool lookup service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint "$fingerprint" \
    >/dev/null 2>&1; then
    if pending_passphrase="$(secret-tool lookup \
      service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint pending 2>/dev/null)"; then
      printf '%s' "$pending_passphrase" | secret-tool store \
        --label='TorrentNG additional Launchpad signing key' \
        service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint "$fingerprint" ||
        fail "Could not recover the additional key passphrase from its pending Secret Service entry."
      secret-tool clear service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint pending >/dev/null ||
        fail "The additional key exists, but its temporary Secret Service entry could not be cleared."
      unset pending_passphrase
    else
      fail "The additional key exists, but its passphrase is missing from Secret Service."
    fi
  fi
  secret-tool lookup service "$ADDITIONAL_KEY_SECRET_SERVICE" fingerprint "$fingerprint" \
    >/dev/null 2>&1 ||
    fail "The additional key exists, but its passphrase is missing from Secret Service."

  if ! printf 'TorrentNG additional Launchpad key check\n' |
    gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3 \
      --local-user "$fingerprint" --detach-sign --output /dev/null \
      3< <(secret-tool lookup service "$ADDITIONAL_KEY_SECRET_SERVICE" \
        fingerprint "$fingerprint"; printf '\n'); then
    fail "The additional key could not sign with its stored Secret Service passphrase."
  fi

  printf 'Additional key: %s\n' "$fingerprint"
  printf 'Publishing its public key to the Ubuntu keyserver...\n'
  gpg --batch --keyserver "$UBUNTU_KEYSERVER" --send-keys "$fingerprint" ||
    fail "Could not publish the additional public key to keyserver.ubuntu.com."

  observed_fingerprint=""
  for attempt in {1..180}; do
    observed_fingerprint="$(keyserver_has_key "$fingerprint" 2>/dev/null || true)"
    if [[ "$observed_fingerprint" == "$fingerprint" ]]; then
      break
    fi
    if (( attempt % 6 == 0 )); then
      printf 'Waiting for keyserver propagation... (%s/180; up to 30 minutes)\n' "$attempt"
    fi
    sleep 10
  done
  [[ "$observed_fingerprint" == "$fingerprint" ]] ||
    fail "The keyserver has not returned the new key yet. Rerun this script later; it will reuse the same key."

  if launchpad_has_key "$fingerprint"; then
    printf 'Launchpad already lists the additional key.\n'
    return
  else
    registered=$?
    [[ "$registered" == 1 ]] || fail "Could not check the additional key's Launchpad registration."
  fi

  printf '\nImport this fingerprint into Launchpad:\n  %s\n' "$fingerprint"
  printf 'Open %s and submit it if you have not already done so.\n' "$LAUNCHPAD_KEY_PAGE"
  if command -v xdg-open >/dev/null 2>&1 &&
    [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
    xdg-open "$LAUNCHPAD_KEY_PAGE" >/dev/null 2>&1 || true
  fi
  IFS= read -r -p 'Press Enter once the import request is submitted or already pending: ' </dev/tty

  printf '\nLaunchpad sends a confirmation link to the new key email address.\n'
  printf 'Open that link. On the "Confirm sign-only OpenPGP key" page, copy the complete challenge paragraph.\n'
  printf 'Paste the paragraph here, then press Enter on an empty line to sign it with the additional key.\n'
  while IFS= read -r message_line; do
    [[ -z "$message_line" ]] && break
    challenge+="$message_line"$'\n'
  done </dev/tty
  [[ -n "$challenge" ]] ||
    fail "No Launchpad challenge text was provided. Rerun the script; it will reuse the same key."

  signed_challenge="$(printf '%s' "$challenge" |
    gpg --batch --yes --armor --clearsign --pinentry-mode loopback --passphrase-fd 3 \
      --local-user "$fingerprint" \
      3< <(secret-tool lookup service "$ADDITIONAL_KEY_SECRET_SERVICE" \
        fingerprint "$fingerprint"; printf '\n'))" ||
    fail "Could not clear-sign Launchpad's challenge with the additional key."
  unset challenge
  printf '\nPaste the complete clear-signed block below into Launchpad and click Continue:\n\n'
  printf '%s\n' "$signed_challenge"
  unset signed_challenge
  IFS= read -r -p 'Press Enter after Launchpad reports that the key was validated: ' </dev/tty

  printf 'Waiting for Launchpad to confirm the additional key...\n'
  for attempt in {1..120}; do
    if launchpad_has_key "$fingerprint"; then
      printf 'Launchpad now lists the additional key.\n'
      return
    else
      registered=$?
      [[ "$registered" == 1 ]] || fail "Could not check the additional key's Launchpad registration."
    fi
    if (( attempt % 12 == 0 )); then
      printf 'Still waiting for Launchpad confirmation... (%s/120)\n' "$attempt"
    fi
    sleep 5
  done
  fail "Launchpad has not confirmed the additional key yet. Rerun this script after completing the confirmation; it will reuse the same key."
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if (( $# > 1 )); then
  usage >&2
  exit 2
fi

for command_name in gh gpg gpgconf awk sed mktemp rm sleep curl python3 secret-tool openssl; do
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

gpg_home="$(gpgconf --list-dirs homedir)" || fail "Could not determine the active GPG home."
secret_listing="$(gpg --batch --with-colons --fingerprint --list-secret-keys \
  "$EXPECTED_FINGERPRINT" 2>/dev/null)" ||
  fail "No Launchpad private key is available in $gpg_home. Import the original private-key backup with 'gpg --import /path/to/key.asc', or set GNUPGHOME to the keyring that contains it. Launchpad stores only the public key."
actual_fingerprint="$(awk -F: '$1 == "fpr" { print $10; exit }' <<< "$secret_listing")"
[[ "$actual_fingerprint" == "$EXPECTED_FINGERPRINT" ]] ||
  fail "The local GPG keyring must contain Launchpad's upload key $EXPECTED_FINGERPRINT."

key_header="$(gpg --batch --armor --export-secret-keys "$EXPECTED_FINGERPRINT" | sed -n '1p')" ||
  fail "Could not export the Launchpad private key from GPG."
[[ "$key_header" == "-----BEGIN PGP PRIVATE KEY BLOCK-----" ]] ||
  fail "GPG did not export a private key block. Import the matching exportable key and try again."

ensure_additional_key

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
