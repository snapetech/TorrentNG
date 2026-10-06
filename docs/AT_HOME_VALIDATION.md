# At-home validation plan

This runbook is for the next validation of the changes on
`codex/credibility-and-release-prep`. No tests, benchmarks, certification jobs,
or live transfers were run while preparing it. Do not cut a release from this
branch until this runbook is complete and the strict release gate passes.
No pull request was opened during preparation because opening one would start
the hosted test and evidence jobs.

Keep generated reports outside the checkout. That leaves the source tree clean
and makes each report identify the exact commit it exercised.

## 1. Start from the pushed commit

From a Linux host with Docker and the target storage available. If the
repository is not already cloned on that host:

```sh
git clone https://github.com/snapetech/torrentng.git
cd torrentng
```

Then update the prepared branch:

```sh
git switch codex/credibility-and-release-prep
git pull --ff-only origin codex/credibility-and-release-prep
git status --short --branch
git rev-parse HEAD
```

Stop if `git status` shows source changes.

## 2. Run the same quality checks as CI

Run these from the repository root. If you redirect output, keep logs in a
private directory outside the checkout.

```sh
cargo fmt --all -- --check
python3 scripts/validate_openapi.py
python3 -m unittest scripts.test_protected_target scripts.test_backend_burndown_api_load scripts.test_workflow_security
cargo test --workspace --all-targets --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --locked --manifest-path sidecar/Cargo.toml
cargo test --locked --manifest-path sidecar/Cargo.toml
cargo clippy --locked --manifest-path sidecar/Cargo.toml --all-targets -- -D warnings
```

Then run the WebUI checks:

```sh
cd webui
npm ci
npm test
npm run build
npm run lint
npx playwright install chromium
npm run test:e2e -- --reporter=line
cd ..
```

CI also runs declared Rust MSRV builds, bounded fuzz smoke, dependency audits,
backup/restore, and fault/load jobs. After the local checks pass, open a pull
request and wait for every required GitHub check before merging. Opening the PR
starts hosted test and evidence jobs; do this when you are ready for those runs.

## 3. Pin hardware evidence to the merged commit

After the required checks pass and the PR is merged, update to the exact `main`
commit that the release would use. Run the hardware and release evidence below
against this clean commit, not the earlier feature-branch SHA:

```sh
git switch main
git pull --ff-only origin main
git status --short --branch
export SOURCE_REVISION="$(git rev-parse HEAD)"
export EVIDENCE_DIR="/tmp/torrentng-evidence-${SOURCE_REVISION:0:12}"
mkdir -p "$EVIDENCE_DIR"
```

Stop if the checkout is dirty. Preserve this full source SHA in the reports.

## 4. Qualify storage on a dedicated test path

Choose an empty mounted path where TorrentNG may create and remove its own test
files. Do not point this at an active download library. This suite qualifies
only the exercised device, filesystem, and workload.

```sh
export TORRENTNG_SOURCE_REVISION="$SOURCE_REVISION"
TNG_STORAGE_REPORT_DIR="$EVIDENCE_DIR" \
TNG_STORAGE_RELEASE_REPORT="$EVIDENCE_DIR/storage-release.md" \
scripts/storage_release_certification.sh /path/to/dedicated/test-mount
```

The script refuses to run its hardware gates unless the source tree is clean.
The Docker image labels carry the full source revision for later report checks.

## 5. Run local and public interoperability when ready

The local matrix uses disposable Docker clients and services:

```sh
REPORT_DIR="$EVIDENCE_DIR" scripts/interop_matrix.sh --local
```

The public matrix contacts public trackers and downloads official Linux
distribution torrents. Run it only when that network traffic and download are
appropriate for the host:

```sh
REPORT_DIR="$EVIDENCE_DIR" scripts/interop_matrix.sh --public
```

Do not treat one successful torrent or one client as proof of universal
compatibility. Review the per-client matrix and failure logs.

## 6. Run the 24-hour idle-daemon soak

Keep the private `.env` token used by the Compose stack. If it does not exist,
create one without replacing an existing token, then build the image with the
commit label and start it:

```sh
if [ ! -f .env ]; then
  umask 077
  printf 'TORRENTNG_API_TOKEN=%s\n' "$(openssl rand -hex 32)" > .env
fi
export TORRENTNG_SOURCE_REVISION="$SOURCE_REVISION"
docker compose --env-file .env -f deploy/native/compose.yml up -d --build
```

Start the soak against that exact container. This idle soak proves service
continuity and process ceilings only; it does not test a busy swarm.

```sh
export TNG_CONTAINER="$(docker compose --env-file .env -f deploy/native/compose.yml ps -q torrentngd)"
export TNG_API_TOKEN="$(sed -n 's/^TORRENTNG_API_TOKEN=//p' .env)"
export SOAK_DATA_PATH=/var/lib/torrentngd
REPORT_DIR="$EVIDENCE_DIR" \
TNG_RUN_DIR="$EVIDENCE_DIR/run" \
scripts/start_24h_soak.sh "$EVIDENCE_DIR/soak-24h-${SOURCE_REVISION:0:12}.md"
```

Check progress with `scripts/soak_status.sh`. After a complete 24-hour run:

```sh
SOAK_MIN_SAMPLES=1200 RESTORE_NORMAL=1 \
scripts/finalize_soak.sh \
  "$EVIDENCE_DIR/soak-24h-${SOURCE_REVISION:0:12}.md" \
  "$EVIDENCE_DIR/soak-final-${SOURCE_REVISION:0:12}.md"
```

The runner now stops before sampling if the checkout is dirty or the running
image's `org.opencontainers.image.revision` label does not match the checked
out commit. The finalizer also rejects reports without matching source and
image revisions.

## 7. Decide whether a release is justified

First produce a strict evidence rollup:

```sh
REPORT_DIR="$EVIDENCE_DIR" scripts/release_evidence_suite.sh
REPORT_DIR="$EVIDENCE_DIR" scripts/release_readiness_gate.sh
```

Review the full reports, not only the final status word. A missing, stale,
skipped, warning, or failed row means a release is not justified. Do not use
the local-only scope to turn missing public or hardware evidence into a pass.
If the strict gate passes, review the exact built artifacts and release notes,
then cut a release from the clean, validated commit.
