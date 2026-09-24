from __future__ import annotations

import re
import json
import os
import stat
import subprocess
import tempfile
import unittest
import base64
import shutil
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"
CHECKOUT_WORKFLOWS = (
    "ci.yml",
    "windows-smoke.yml",
    "backend-evidence.yml",
    "release.yml",
    "release-aur.yml",
    "release-copr.yml",
    "package-smoke-disabled.yml",
)


def run_scripts(source: str) -> list[str]:
    lines = source.splitlines()
    scripts: list[str] = []
    for index, line in enumerate(lines):
        match = re.match(r"^(?P<indent>\s*)run:\s*(?P<value>.*)$", line)
        if match is None:
            continue
        indent = len(match.group("indent"))
        value = match.group("value")
        body = [value]
        if value in {"", "|", "|-", "|+", ">", ">-", ">+"}:
            for child in lines[index + 1 :]:
                if child.strip() and len(child) - len(child.lstrip()) <= indent:
                    break
                body.append(child)
        scripts.append("\n".join(body))
    return scripts


def run_interop_preflight(
    script: Path, temp_path: Path, overrides: dict[str, str]
) -> tuple[subprocess.CompletedProcess[str], Path]:
    bin_path = temp_path / "bin"
    bin_path.mkdir(parents=True, exist_ok=True)
    docker_log = temp_path / "docker.log"
    stubs = {
        "docker": (
            "#!/bin/sh\n"
            "printf '%s\\n' \"$*\" >>\"$DOCKER_LOG\"\n"
            "exit 23\n"
        ),
        "jq": "#!/bin/sh\nexit 0\n",
        "base64": "#!/bin/sh\nexit 0\n",
    }
    for name, contents in stubs.items():
        executable = bin_path / name
        executable.write_text(contents, encoding="utf-8")
        executable.chmod(0o700)
    environment = os.environ.copy()
    environment.update(
        {
            "PATH": f"{bin_path}:{environment['PATH']}",
            "DOCKER_LOG": str(docker_log),
            **overrides,
        }
    )
    completed = subprocess.run(
        ["bash", str(script), "--local"],
        capture_output=True,
        text=True,
        env=environment,
    )
    return completed, docker_log


class WorkflowSecurityTests(unittest.TestCase):
    def test_public_pull_requests_never_select_persistent_runners(self) -> None:
        ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertRegex(ci, r"(?m)^\s*pull_request:")
        self.assertNotRegex(ci, r"(?m)^\s*runs-on:\s*\[self-hosted")
        self.assertEqual(ci.count("github.event_name == 'push' && fromJSON"), 6)
        self.assertNotRegex(ci, r"(?m)^\s*pull_request_target:")

        windows = (WORKFLOWS / "windows-smoke.yml").read_text(encoding="utf-8")
        self.assertRegex(windows, r"(?m)^\s*runs-on:\s*windows-latest\s*$")
        self.assertNotRegex(windows, r"(?m)^\s*runs-on:.*self-hosted")

    def test_checkout_credentials_are_not_persisted(self) -> None:
        for workflow_name in CHECKOUT_WORKFLOWS:
            source = (WORKFLOWS / workflow_name).read_text(encoding="utf-8")
            lines = source.splitlines()
            checkout_lines = [
                index for index, line in enumerate(lines) if "uses: actions/checkout@" in line
            ]
            self.assertTrue(checkout_lines, workflow_name)
            for index in checkout_lines:
                block = "\n".join(lines[index : index + 6])
                self.assertIn("persist-credentials: false", block, f"{workflow_name}:{index + 1}")

    def test_untrusted_values_are_not_interpolated_into_shell(self) -> None:
        tainted = re.compile(
            r"\$\{\{\s*(?:inputs\.|github\.event\.(?:inputs|release\.)|"
            r"steps\.(?:tag|version)\.outputs\.(?:tag|rpm_version))"
        )
        for workflow in WORKFLOWS.glob("*.yml"):
            source = workflow.read_text(encoding="utf-8")
            for index, script in enumerate(run_scripts(source), start=1):
                self.assertIsNone(
                    tainted.search(script),
                    f"untrusted context is interpolated into a run step in {workflow.name}, run block {index}",
                )

    def test_release_tags_are_validated_before_release_jobs(self) -> None:
        release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        self.assertIn("validate-release-tag:", release)
        self.assertIn("REQUESTED_TAG: ${{ inputs.tag || github.ref_name }}", release)
        self.assertIn("git merge-base --is-ancestor", release)
        self.assertIn("needs: validate-release-tag", release)

    def test_bearer_targets_and_soak_transport_fail_closed(self) -> None:
        api_load = (ROOT / "scripts" / "backend_burndown_api_load.py").read_text(encoding="utf-8")
        self.assertIn("NoProxyHandler", api_load)
        self.assertIn("NoRedirectHandler", api_load)
        self.assertIn("validate_protected_target(requested_base)", api_load)

        soak = (ROOT / "scripts" / "soak_certification.sh").read_text(encoding="utf-8")
        self.assertIn('python3 "$ROOT/scripts/protected_target.py" "$TNG_HOST_URL"', soak)
        self.assertIn("curl -q --silent --show-error --noproxy '*'", soak)
        self.assertNotIn("curl -ksS", soak)
        self.assertNotIn('Authorization: Bearer $TNG_API_TOKEN', soak)
        self.assertIn('auth_payload="$(python3 -c', soak)
        self.assertIn('-H "@$AUTH_HEADER_FILE"', soak)

    def test_certification_scripts_verify_tls_and_validate_secret_targets(self) -> None:
        for script in (ROOT / "scripts").glob("*.sh"):
            source = script.read_text(encoding="utf-8")
            self.assertNotRegex(source, r"\bcurl\s+-[^\s]*k")
            self.assertNotRegex(source, r"\bcurl\s+--insecure\b")

        for script_name in (
            "live_certification.sh",
            "live_transfer_certification.sh",
            "transfer_churn_soak.sh",
            "mobile_compat_certification.sh",
            "autobrr_certification.sh",
            "arr_app_certification.sh",
            "app_add_job_certification.sh",
            "configure_certification_clients.sh",
            "dht_certification.sh",
        ):
            source = (ROOT / "scripts" / script_name).read_text(encoding="utf-8")
            self.assertIn("protected_target.py", source, script_name)

    def test_nginx_proxy_drops_client_supplied_identity_header(self) -> None:
        source = (ROOT / "deploy" / "nginx" / "nginx.conf").read_text(encoding="utf-8")
        locations = re.findall(
            r"(?ms)^[ \t]*location\b[^{}]*\{(.*?)^[ \t]*\}", source
        )
        proxied = [
            block for block in locations if re.search(r"(?m)^[ \t]*proxy_pass\b", block)
        ]
        self.assertGreaterEqual(len(proxied), 3)
        for index, block in enumerate(proxied, start=1):
            with self.subTest(location=index):
                self.assertRegex(
                    block,
                    r'(?m)^[ \t]*proxy_set_header\s+X-Remote-User\s+"";[ \t]*$',
                )

    def test_http_management_ports_are_loopback_bound_by_default(self) -> None:
        bindings = (
            ("deploy/docker/compose.yml", "127.0.0.1:8080:8080"),
            ("deploy/docker/compose.yml", "127.0.0.1:80:80"),
            ("deploy/docker/compose.qbittorrent.yml", "127.0.0.1:8082:8080"),
            ("deploy/docker/compose.transmission.yml", "127.0.0.1:8083:8080"),
            ("deploy/docker/compose.deluge.yml", "127.0.0.1:8084:8080"),
            ("deploy/native/compose.yml", "127.0.0.1:28082:8080"),
        )
        for relative_path, binding in bindings:
            with self.subTest(path=relative_path, binding=binding):
                source = (ROOT / relative_path).read_text(encoding="utf-8")
                self.assertIn(f'- "{binding}"', source)

    def test_native_deployments_pin_non_root_privileges(self) -> None:
        dockerfile = (ROOT / "deploy/native/Dockerfile").read_text(encoding="utf-8")
        identity = (ROOT / "deploy/container/identity.sh").read_text(encoding="utf-8")
        compose = (ROOT / "deploy/native/compose.yml").read_text(encoding="utf-8")
        interop = (ROOT / "deploy/interop/compose.yml").read_text(encoding="utf-8")
        statefulset = (ROOT / "deploy/native/kubernetes/statefulset.yaml").read_text(
            encoding="utf-8"
        )

        self.assertIn('TNG_UID must be a nonzero numeric ID', dockerfile)
        self.assertIn('TNG_GID must be a nonzero numeric ID', dockerfile)
        self.assertIn('USER root', dockerfile)
        self.assertIn('tng_identity_enter', identity)
        self.assertIn('--bounding-set=-all', identity)
        self.assertIn('PUID', identity)
        self.assertIn('PGID', identity)
        for source in (compose, interop):
            self.assertIn("cap_drop:\n      - ALL", source)
            self.assertIn("no-new-privileges:true", source)
            for capability in ("CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"):
                self.assertIn(capability, source)
        self.assertIn("runAsNonRoot: true", statefulset)
        self.assertIn("runAsUser: 1000", statefulset)
        self.assertIn("runAsGroup: 1000", statefulset)
        self.assertIn("allowPrivilegeEscalation: false", statefulset)
        self.assertIn("type: RuntimeDefault", statefulset)
        self.assertIn("automountServiceAccountToken: false", statefulset)

    def test_default_rtorrent_settings_overlay_uses_persistent_writable_state(self) -> None:
        compose = (ROOT / "deploy/docker/compose.yml").read_text(encoding="utf-8")
        entrypoint = (ROOT / "deploy/docker/entrypoint.sh").read_text(encoding="utf-8")
        self.assertIn("./config:/config:ro", compose)
        self.assertIn(
            "TNG_RTORRENT_OVERLAY: /var/lib/torrentng/rtorrent-ui-overlay.rc",
            compose,
        )
        self.assertIn('set -C; : > "$RTORRENT_UI_OVERLAY"', entrypoint)
        self.assertIn('grep -Fqx "import = $RTORRENT_UI_OVERLAY"', entrypoint)
        self.assertIn('printf \'\\nimport = %s\\n\'', entrypoint)

    def test_arr_fixture_never_deletes_a_fixed_download_directory(self) -> None:
        source = (ROOT / "scripts" / "arr_app_certification.sh").read_text(encoding="utf-8")
        self.assertIn("FIXTURE_DOWNLOAD_DIR=\"cert-arr-fixture-$FIXTURE_ID\"", source)
        self.assertNotIn("rm -rf /downloads/cert-arr-fixture", source)
        self.assertNotIn("/downloads/cert-arr-fixture/", source)

    def test_transfer_fixtures_are_run_scoped_and_bounded(self) -> None:
        live = (ROOT / "scripts" / "live_transfer_certification.sh").read_text(encoding="utf-8")
        app = (ROOT / "scripts" / "app_add_job_certification.sh").read_text(encoding="utf-8")
        churn = (ROOT / "scripts" / "transfer_churn_soak.sh").read_text(encoding="utf-8")

        self.assertIn('FIXTURE_DOWNLOAD_DIR="cert-fixture-$FIXTURE_ID"', live)
        self.assertIn('LOCAL_CATEGORY="cert-local-fixture-$FIXTURE_ID"', live)
        self.assertIn('PUBLIC_CATEGORY="cert-public-$FIXTURE_ID"', live)
        self.assertNotIn("rm -rf /downloads/cert-fixture", live)
        self.assertIn('FIXTURE_DOWNLOAD_DIR="cert-app-$FIXTURE_ID"', app)
        self.assertNotIn("rm -rf /downloads/cert-fixture", app)
        self.assertIn('CHURN_DOWNLOAD_DIR="transfer-churn-$FIXTURE_ID"', churn)
        self.assertIn('CATEGORY="cert-transfer-churn-$FIXTURE_ID"', churn)
        self.assertNotIn("rm -rf /downloads/transfer-churn/$cycle", churn)
        for source in (live, app, churn):
            self.assertIn("67108864", source)
            self.assertIn("tng_write_qbit_login_body", source)

    def test_fixture_identifiers_and_sizes_fail_closed_before_docker(self) -> None:
        cases = (
            (
                "live_transfer_certification.sh",
                {"CERT_FIXTURE_ID": "bad;touch-this", "TNG_HOST_URL": "http://localhost:18080"},
            ),
            ("app_add_job_certification.sh", {"FIXTURE_BYTES": "1048576;touch-this"}),
            ("transfer_churn_soak.sh", {"TRANSFER_CHURN_ID": "../outside"}),
            ("arr_app_certification.sh", {"FIXTURE_BYTES": "999999999"}),
        )
        for script_name, overrides in cases:
            with self.subTest(script=script_name):
                environment = os.environ.copy()
                environment.update({"CERT_ENV_FILE": "/dev/null", **overrides})
                completed = subprocess.run(
                    ["bash", str(ROOT / "scripts" / script_name)],
                    capture_output=True,
                    text=True,
                    env=environment,
                )
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertNotIn("docker: command not found", completed.stderr)

    def test_curl_policy_keeps_secrets_out_of_child_argv(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            fake_curl = bin_path / "curl"
            fake_curl.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, stat, sys\n"
                "args = sys.argv[1:]\n"
                "loaded = []\n"
                "for index, value in enumerate(args[:-1]):\n"
                "    if value in ('-H', '--header') and args[index + 1].startswith('@'):\n"
                "        path = args[index + 1][1:]\n"
                "        loaded.append((path, open(path, encoding='utf-8').read(), stat.S_IMODE(os.stat(path).st_mode)))\n"
                "    if value in ('-d', '--data', '--data-binary', '--data-urlencode', '--json'):\n"
                "        body = args[index + 1]\n"
                "        path = body.split('@', 1)[1] if '@' in body else ''\n"
                "        if path and os.path.isfile(path):\n"
                "            loaded.append((path, open(path, encoding='utf-8').read(), stat.S_IMODE(os.stat(path).st_mode)))\n"
                "print(json.dumps({'args': args, 'loaded': loaded}))\n",
                encoding="utf-8",
            )
            fake_curl.chmod(0o700)
            cookie_jar = temp_path / "cookies.txt"
            cookie_jar.write_text("# Netscape HTTP Cookie File\n", encoding="utf-8")
            cookie_jar.chmod(0o644)
            cookie_input = temp_path / "cookie-input.txt"
            cookie_input.write_text("sid=input-cookie-secret-22\n", encoding="utf-8")
            cookie_input.chmod(0o644)
            netrc_input = temp_path / "netrc-input.txt"
            netrc_input.write_text(
                "machine localhost login api password netrc-secret-23\n",
                encoding="utf-8",
            )
            netrc_input.chmod(0o644)
            default_netrc = temp_path / ".netrc"
            default_netrc.write_text(
                "machine localhost login api password netrc-secret-24\n",
                encoding="utf-8",
            )
            default_netrc.chmod(0o644)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "TMPDIR": temporary,
                    "HOME": temporary,
                    "NETRC": "",
                    "TEST_BEARER": "bearer-secret-91",
                    "TEST_API_KEY": "apikey-secret-73",
                    "TEST_BODY": '{"token":"body-secret-58"}',
                    "TEST_JSON": '{"refresh_secret":"json-secret-81"}',
                    "TEST_JSON_EQUALS": '{"refresh_token":"json-secret-82"}',
                    "TEST_QUERY": "query-secret-26",
                    "TEST_CUSTOM_SECRET": "custom-secret-63",
                    "TEST_SESSION_SECRET": "session-secret-37",
                    "TEST_REFERER": "https://origin.invalid/article?session=referer-secret-19",
                    "TEST_REFERER_EQUALS": "https://origin.invalid/story?token=referer-secret-20",
                    "TEST_REFERER_SHORT": "https://origin.invalid/page?key=referer-secret-21",
                    "CURL_OUTPUT": str(temp_path / "response.json"),
                    "CURL_COOKIE_JAR": str(cookie_jar),
                    "CURL_COOKIE_INPUT": str(cookie_input),
                    "CURL_NETRC_INPUT": str(netrc_input),
                }
            )
            command = (
                'source "$1"; '
                'curl -fsS -H "Authorization: Bearer $TEST_BEARER" '
                '-H "X-Api-Key: $TEST_API_KEY" '
                '-H "X-Client-Secret: $TEST_CUSTOM_SECRET" '
                '-H " X-Session-Token: $TEST_SESSION_SECRET" '
                '-H "Referer: $TEST_REFERER" --referer="$TEST_REFERER_EQUALS" '
                '-e"$TEST_REFERER_SHORT" '
                '-H "Content-Type: application/json" '
                '-d "$TEST_BODY" --data-urlencode "query=$TEST_QUERY" '
                '--json "$TEST_JSON" --json="$TEST_JSON_EQUALS" '
                '-o "$CURL_OUTPUT" -w \'%{http_code}\' '
                '--write-out=\'%{time_total}\' -X POST '
                '-b "$CURL_COOKIE_INPUT" -c "$CURL_COOKIE_JAR" '
                '--netrc-file="$CURL_NETRC_INPUT" http://localhost/api'
            )
            completed = subprocess.run(
                ["bash", "-c", command, "bash", str(helper)],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

            result = json.loads(completed.stdout)
            args = result["args"]
            argv_text = " ".join(args)
            for secret in (
                environment["TEST_BEARER"],
                environment["TEST_API_KEY"],
                environment["TEST_CUSTOM_SECRET"],
                environment["TEST_SESSION_SECRET"],
                "referer-secret-19",
                "referer-secret-20",
                "referer-secret-21",
                "body-secret-58",
                "json-secret-81",
                "json-secret-82",
                "input-cookie-secret-22",
                "netrc-secret-23",
                "netrc-secret-24",
                environment["TEST_QUERY"],
            ):
                self.assertNotIn(secret, argv_text)
            self.assertEqual(args[0], "-q")
            self.assertIn("--noproxy", args)
            self.assertIn("--proto-redir", args)
            self.assertEqual(len(result["loaded"]), 5)
            self.assertEqual(
                stat.S_IMODE(cookie_jar.stat().st_mode), stat.S_IRUSR | stat.S_IWUSR
            )
            self.assertEqual(
                stat.S_IMODE(cookie_input.stat().st_mode), stat.S_IRUSR | stat.S_IWUSR
            )
            self.assertEqual(cookie_input.read_text(encoding="utf-8"), "sid=input-cookie-secret-22\n")
            self.assertEqual(
                stat.S_IMODE(netrc_input.stat().st_mode), stat.S_IRUSR | stat.S_IWUSR
            )
            self.assertEqual(
                netrc_input.read_text(encoding="utf-8"),
                "machine localhost login api password netrc-secret-23\n",
            )
            self.assertIn("Authorization: Bearer bearer-secret-91", result["loaded"][0][1])
            self.assertIn("X-Api-Key: apikey-secret-73", result["loaded"][0][1])
            self.assertIn("X-Client-Secret: custom-secret-63", result["loaded"][0][1])
            self.assertIn(" X-Session-Token: session-secret-37", result["loaded"][0][1])
            self.assertIn(f"Referer: {environment['TEST_REFERER']}", result["loaded"][0][1])
            self.assertIn(f"Referer: {environment['TEST_REFERER_EQUALS']}", result["loaded"][0][1])
            self.assertIn(f"Referer: {environment['TEST_REFERER_SHORT']}", result["loaded"][0][1])
            self.assertTrue(any(body == '{"token":"body-secret-58"}' for _, body, _ in result["loaded"]))
            self.assertTrue(any(body == '{"refresh_secret":"json-secret-81"}' for _, body, _ in result["loaded"]))
            self.assertTrue(any(body == '{"refresh_token":"json-secret-82"}' for _, body, _ in result["loaded"]))
            self.assertTrue(any(body == "query-secret-26" for _, body, _ in result["loaded"]))
            for path, _, mode in result["loaded"]:
                self.assertEqual(mode, stat.S_IRUSR | stat.S_IWUSR)
                self.assertFalse(Path(path).exists())

            for option in ("--netrc", "--netrc-optional"):
                default_netrc_call = subprocess.run(
                    [
                        "bash",
                        "-c",
                        f'source "$1"; curl {option} http://localhost',
                        "bash",
                        str(helper),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    env=environment,
                )
                self.assertNotIn("netrc-secret-24", default_netrc_call.stdout)
                self.assertEqual(
                    stat.S_IMODE(default_netrc.stat().st_mode),
                    stat.S_IRUSR | stat.S_IWUSR,
                )

    def test_curl_policy_rejects_redirects_and_proxy_overrides_for_secrets(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        command = 'source "$1"; curl -H "Authorization: Bearer $TEST_TOKEN" -L http://example.invalid'
        environment = os.environ.copy()
        environment["TEST_TOKEN"] = "not-for-argv"
        completed = subprocess.run(
            ["bash", "-c", command, "bash", str(helper)],
            capture_output=True,
            text=True,
            env=environment,
        )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("redirects are disabled", completed.stderr)

        proxy_command = 'source "$1"; curl --proxy http://proxy.invalid http://localhost'
        completed = subprocess.run(
            ["bash", "-c", proxy_command, "bash", str(helper)],
            capture_output=True,
            text=True,
            env=environment,
        )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("explicit proxy routing is disabled", completed.stderr)

    def test_curl_policy_preserves_sensitive_header_files(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            fake_curl = bin_path / "curl"
            fake_curl.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, stat, sys\n"
                "args = sys.argv[1:]\n"
                "loaded = []\n"
                "for index, value in enumerate(args[:-1]):\n"
                "    if value in ('-H', '--header') and args[index + 1].startswith('@'):\n"
                "        path = args[index + 1][1:]\n"
                "        loaded.append((path, open(path, encoding='utf-8').read(), stat.S_IMODE(os.stat(path).st_mode)))\n"
                "print(json.dumps({'args': args, 'loaded': loaded}))\n",
                encoding="utf-8",
            )
            fake_curl.chmod(0o700)
            source_headers = temp_path / "auth.headers"
            source_headers.write_text(
                "Authorization: Bearer header-file-secret-42\n"
                "X-Api-Secret: file-secret-52\n"
                "X-Cert-Run: private\n",
                encoding="utf-8",
            )
            source_headers.chmod(0o600)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "TMPDIR": temporary,
                    "TEST_HEADER_FILE": str(source_headers),
                }
            )
            completed = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; curl -H "@$TEST_HEADER_FILE" http://localhost/api',
                    "bash",
                    str(helper),
                ],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

            result = json.loads(completed.stdout)
            self.assertNotIn("header-file-secret-42", " ".join(result["args"]))
            self.assertEqual(len(result["loaded"]), 1)
            path, copied_headers, mode = result["loaded"][0]
            self.assertIn("Authorization: Bearer header-file-secret-42", copied_headers)
            self.assertIn("X-Api-Secret: file-secret-52", copied_headers)
            self.assertIn("X-Cert-Run: private", copied_headers)
            self.assertEqual(mode, stat.S_IRUSR | stat.S_IWUSR)
            self.assertFalse(Path(path).exists())
            self.assertTrue(source_headers.exists())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO support required")
    def test_curl_policy_rejects_special_header_files_before_curl(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            fake_curl = bin_path / "curl"
            fake_curl.write_text(
                "#!/usr/bin/env python3\n"
                "import os\n"
                "from pathlib import Path\n"
                "Path(os.environ['CURL_MARKER']).write_text('called', encoding='utf-8')\n",
                encoding="utf-8",
            )
            fake_curl.chmod(0o700)
            fifo = temp_path / "headers.fifo"
            os.mkfifo(fifo)
            header_target = temp_path / "headers.txt"
            header_target.write_text("X-Cert-Run: private\n", encoding="utf-8")
            header_symlink = temp_path / "headers-link"
            header_symlink.symlink_to(header_target)
            marker = temp_path / "curl-was-called"
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "CURL_MARKER": str(marker),
                }
            )

            for header_source in (fifo, header_symlink):
                with self.subTest(header_source=header_source.name):
                    environment["TEST_HEADER_FILE"] = str(header_source)
                    completed = subprocess.run(
                        [
                            "bash",
                            "-c",
                            'source "$1"; curl -H "@$TEST_HEADER_FILE" http://localhost/api',
                            "bash",
                            str(helper),
                        ],
                        capture_output=True,
                        text=True,
                        env=environment,
                        timeout=3,
                    )

                    self.assertEqual(completed.returncode, 2)
                    self.assertIn(
                        "sensitive header file is not a readable regular file",
                        completed.stderr,
                    )
                    self.assertFalse(marker.exists(), "curl must not run for an unsafe header source")

    def test_curl_policy_spools_multipart_and_other_credential_options(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            fake_curl = bin_path / "curl"
            fake_curl.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, stat, sys\n"
                "args = sys.argv[1:]\n"
                "loaded = []\n"
                "for index, value in enumerate(args[:-1]):\n"
                "    if value in ('-H', '--header') and args[index + 1].startswith('@'):\n"
                "        path = args[index + 1][1:]\n"
                "        loaded.append((path, open(path, encoding='utf-8').read(), stat.S_IMODE(os.stat(path).st_mode)))\n"
                "    if value in ('-F', '--form') and '<' in args[index + 1]:\n"
                "        path = args[index + 1].split('<', 1)[1]\n"
                "        loaded.append((path, open(path, encoding='utf-8').read(), stat.S_IMODE(os.stat(path).st_mode)))\n"
                "print(json.dumps({'args': args, 'loaded': loaded}))\n",
                encoding="utf-8",
            )
            fake_curl.chmod(0o700)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "TMPDIR": temporary,
                    "TEST_FORM_SECRET": "magnet-secret-81",
                    "TEST_BASIC_SECRET": "cert-user:cert-password-17",
                    "TEST_OAUTH_SECRET": "oauth-secret-29",
                    "TEST_COOKIE_SECRET": "sid=cookie-secret-33",
                }
            )
            command = (
                'source "$1"; '
                'curl --form-string "urls=$TEST_FORM_SECRET" '
                '--user "$TEST_BASIC_SECRET" --oauth2-bearer "$TEST_OAUTH_SECRET" '
                '-b "$TEST_COOKIE_SECRET" http://localhost/api'
            )
            completed = subprocess.run(
                ["bash", "-c", command, "bash", str(helper)],
                check=True,
                capture_output=True,
                text=True,
                env=environment,
            )

            result = json.loads(completed.stdout)
            arguments = " ".join(result["args"])
            for secret in (
                "magnet-secret-81",
                "cert-user:cert-password-17",
                "oauth-secret-29",
                "sid=cookie-secret-33",
            ):
                self.assertNotIn(secret, arguments)
            headers = "\n".join(contents for _, contents, _ in result["loaded"] if "Authorization:" in contents or "Cookie:" in contents)
            self.assertIn(
                "Authorization: Basic "
                + base64.b64encode(b"cert-user:cert-password-17").decode("ascii"),
                headers,
            )
            self.assertIn("Authorization: Bearer oauth-secret-29", headers)
            self.assertIn("Cookie: sid=cookie-secret-33", headers)
            self.assertTrue(
                any(contents == "magnet-secret-81" for _, contents, _ in result["loaded"])
            )
            for path, _, mode in result["loaded"]:
                self.assertEqual(mode, stat.S_IRUSR | stat.S_IWUSR)
                self.assertFalse(Path(path).exists())

    def test_curl_policy_rejects_protocol_overrides_and_multipart_redirects(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        for command, message in (
            ('curl --proto =ftp http://localhost', "protocol restrictions are disabled"),
            ('curl -sk http://localhost', "TLS certificate verification cannot be disabled"),
            (
                'curl --resolve localhost:80:127.0.0.1 http://localhost',
                "explicit route overrides are disabled",
            ),
            (
                'curl --doh-url https://resolver.invalid/dns-query http://localhost',
                "explicit route overrides are disabled",
            ),
            (
                'curl --alt-svc cache.txt http://localhost',
                "explicit route overrides are disabled",
            ),
            (
                'curl -H "Proxy-Authorization: Basic c2VjcmV0" http://localhost',
                "Proxy-Authorization headers are disabled",
            ),
            (
                'curl -F "urls=secret" -L http://localhost',
                "redirects are disabled",
            ),
        ):
            with self.subTest(command=command):
                completed = subprocess.run(
                    ["bash", "-c", f'source "$1"; {command}', "bash", str(helper)],
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(completed.returncode, 2)
                self.assertIn(message, completed.stderr)

    def test_curl_policy_rejects_option_resets_and_url_credentials(self) -> None:
        helper = ROOT / "scripts" / "curl_policy.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            bin_path.mkdir()
            marker = temp_path / "curl-was-invoked"
            fake_curl = bin_path / "curl"
            fake_curl.write_text(
                "#!/bin/sh\nprintf 'invoked\\n' > \"$CURL_MARKER\"\nprintf '%s\\n' \"$@\" >> \"$CURL_MARKER\"\n",
                encoding="utf-8",
            )
            fake_curl.chmod(0o700)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "CURL_MARKER": str(marker),
                }
            )
            for command, message in (
                ('curl --next ftp://example.invalid', "multiple transfer groups are disabled"),
                ('curl -: file:///etc/passwd', "multiple transfer groups are disabled"),
                (
                    'curl -H "Authorization: Bearer secret" http://localhost/one http://other.invalid/two',
                    "exactly one explicit HTTP(S) URL",
                ),
                (
                    'curl -H "Authorization: Bearer secret" https://localhost/one other.invalid/two',
                    "exactly one explicit HTTP(S) URL",
                ),
                (
                    'curl -H "Authorization: Bearer secret" https://localhost/one otherhost',
                    "exactly one explicit HTTP(S) URL",
                ),
                (
                    'curl --url @urls.txt',
                    "URL lists from files are disabled",
                ),
                (
                    'curl --url-query access_token=query-secret http://localhost',
                    "inline URL-query arguments are disabled",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -G --data-urlencode "api_key=query-secret" https://localhost/api',
                    "credential-like query fields are disabled",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -Gs --data-urlencode "api%5Fkey=query-secret" https://localhost/api',
                    "credential-like query fields are disabled",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -G --data-urlencode "search@request.txt" https://localhost/api',
                    "file-backed GET data cannot be inspected safely",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --data-urlencode=search@request.txt -Gs https://localhost/api',
                    "file-backed GET data cannot be inspected safely",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --get --data-binary @request.txt https://localhost/api',
                    "file-backed GET data cannot be inspected",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -G --json="{\\"api_key\\":\\"query-secret\\"}" https://localhost/api',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -G --json @request.json https://localhost/api',
                    "file-backed GET data cannot be inspected",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -c - https://localhost/api',
                    "cookie jars must not target stdout",
                ),
                (
                    'curl -b /dev/stdin http://localhost/api',
                    "cookie input must not target a device or descriptor path",
                ),
                (
                    'curl --netrc-file=/dev/stdin http://localhost/api',
                    "netrc credentials must come from an existing owned regular file",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --no-globoff "https://localhost/{one,two}"',
                    "URL globbing cannot be enabled",
                ),
                (
                    'curl --location-trusted http://localhost',
                    "credential-forwarding redirects are disabled",
                ),
                (
                    'curl -e https://origin.invalid/page -L http://localhost',
                    "redirects are disabled for requests with authentication headers",
                ),
                (
                    'curl --referer=https://origin.invalid/page\\;auto http://localhost',
                    "automatic Referer redirects are disabled",
                ),
                (
                    'curl --netrc-optional --follow http://localhost',
                    "redirects are disabled",
                ),
                (
                    'curl --variable %TOKEN --expand-header "Authorization: Bearer {{TOKEN}}" http://localhost',
                    "curl variable expansion is disabled",
                ),
                (
                    'curl --libcurl - -H "Authorization: Bearer secret" https://localhost/api',
                    "generated libcurl source can contain credentials",
                ),
                (
                    'curl --pass private-passphrase https://localhost',
                    "command-line client credential material is disabled",
                ),
                (
                    'curl --cert client.pem:private-passphrase https://localhost',
                    "command-line client credential material is disabled",
                ),
                (
                    'curl --httpsig-key private-signing-key https://localhost',
                    "command-line client credential material is disabled",
                ),
                (
                    'curl -v -H "Authorization: Bearer secret" http://localhost',
                    "diagnostic/header output is disabled",
                ),
                (
                    'curl -H "Authorization: Bearer secret" -w "%{header_json}" http://localhost',
                    "authenticated write-out is limited to HTTP status and elapsed time",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --write-out "%header{Set-Cookie}" http://localhost',
                    "authenticated write-out is limited to HTTP status and elapsed time",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --write-out @writeout.txt http://localhost',
                    "authenticated write-out is limited to HTTP status and elapsed time",
                ),
                (
                    'curl -H "Authorization: Bearer secret" --write-out="%{json}" http://localhost',
                    "authenticated write-out is limited to HTTP status and elapsed time",
                ),
                (
                    'curl --include -b "sid=secret" http://localhost',
                    "diagnostic/header output is disabled",
                ),
                (
                    'curl https://cert-user:cert-password@example.invalid',
                    "URL userinfo credentials are disabled",
                ),
                (
                    'curl ftp://cert-user:cert-password@example.invalid',
                    "URL userinfo credentials are disabled",
                ),
                (
                    'curl cert-user:cert-password@example.invalid',
                    "URL userinfo credentials are disabled",
                ),
                (
                    'curl --url=https://cert-user:cert-password@example.invalid',
                    "URL userinfo credentials are disabled",
                ),
                (
                    'curl --url HTTPS://cert-user:cert-password@example.invalid',
                    "URL userinfo credentials are disabled",
                ),
                (
                    'curl https://example.invalid/api?api_key=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?api%5Fkey=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?%2570asskey=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl https://example.invalid/api?pass=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl https://example.invalid/api?torrent_pass=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?tracker%255Fpass=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?p.a.s.s.k.e.y=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?secret_key=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?pwd=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?pid=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?uk=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl --url=https://example.invalid/api?sig=query-secret',
                    "sensitive URL query parameters are disabled",
                ),
                (
                    'curl -G --data-urlencode "%2570asskey=query-secret" http://localhost',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl -G --data-urlencode "passwd=query-secret" http://localhost',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl -G --data-urlencode "tracker_pass=query-secret" http://localhost',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl -G --data-urlencode "access.t.o.k.e.n=query-secret" http://localhost',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl -G --data-urlencode "pwd=query-secret" http://localhost',
                    "credential-like query fields are disabled for GET data",
                ),
                (
                    'curl https://example.invalid/api?hash=public#fragment',
                    "URL fragments are disabled",
                ),
            ):
                with self.subTest(command=command):
                    completed = subprocess.run(
                        [
                            "bash",
                            "-c",
                            f'source "$1"; {command}',
                            "bash",
                            str(helper),
                        ],
                        capture_output=True,
                        text=True,
                        env=environment,
                    )
                    self.assertEqual(completed.returncode, 2)
                    self.assertIn(message, completed.stderr)
                    self.assertFalse(
                        marker.exists(), "curl ran after a policy bypass argument"
                    )
                    marker.unlink(missing_ok=True)

            allowed = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; curl --url "http://localhost/api?hash=public"',
                    "bash",
                    str(helper),
                ],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(allowed.returncode, 0, allowed.stderr)
            self.assertTrue(marker.exists())

            preserved_url = "https://Example.invalid/CaseSensitive?hash=AbC"
            case_preservation = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; curl --url="$TEST_URL"',
                    "bash",
                    str(helper),
                ],
                capture_output=True,
                text=True,
                env={**environment, "TEST_URL": preserved_url},
            )
            self.assertEqual(case_preservation.returncode, 0, case_preservation.stderr)
            self.assertEqual(
                marker.read_text(encoding="utf-8").splitlines()[-2:],
                ["--url", preserved_url],
            )

            safe_get = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; curl -H "Authorization: Bearer secret" -G '
                    '--data-urlencode "query=public" https://localhost/api',
                    "bash",
                    str(helper),
                ],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(safe_get.returncode, 0, safe_get.stderr)
            self.assertTrue(marker.exists())

            file_backed_post = subprocess.run(
                [
                    "bash",
                    "-c",
                    'source "$1"; curl -H "Authorization: Bearer secret" '
                    '--data-urlencode "search@request.txt" https://localhost/api',
                    "bash",
                    str(helper),
                ],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(file_backed_post.returncode, 0, file_backed_post.stderr)
            self.assertEqual(
                marker.read_text(encoding="utf-8").splitlines()[-3:],
                ["--data-urlencode", "search@request.txt", "https://localhost/api"],
            )

    def test_rtorrent_staging_preserves_nested_names_and_rejects_escape_paths(self) -> None:
        helper = ROOT / "scripts" / "stage_rtorrent_migration.py"
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            snapshot = base / "snapshot"
            staging = base / "staging"
            snapshot.mkdir()
            staging.mkdir()
            report_path = base / "report.json"
            manifest = base / "selected.nul"

            entries = []
            for folder, body in (("one", "torrent-one"), ("two", "torrent-two")):
                directory = snapshot / folder
                directory.mkdir()
                torrent = directory / "same.torrent"
                resume = directory / "same.rtorrent"
                torrent.write_text(body, encoding="utf-8")
                resume.write_text(f"resume-{folder}", encoding="utf-8")
                entries.append(
                    {"torrent_path": str(torrent), "resume_path": str(resume)}
                )
            report_path.write_text(json.dumps({"torrents": entries}), encoding="utf-8")

            completed = subprocess.run(
                ["python3", str(helper), str(report_path), str(snapshot), str(staging), str(manifest)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual((staging / "one" / "same.torrent").read_text(encoding="utf-8"), "torrent-one")
            self.assertEqual((staging / "two" / "same.torrent").read_text(encoding="utf-8"), "torrent-two")
            self.assertEqual(len(manifest.read_bytes().split(b"\0")[:-1]), 4)

            outside = base / "outside.torrent"
            outside.write_text("outside", encoding="utf-8")
            report_path.write_text(
                json.dumps({"torrents": [{"torrent_path": str(outside)}]}),
                encoding="utf-8",
            )
            empty_staging = base / "empty-staging"
            empty_staging.mkdir()
            outside_manifest = base / "outside.nul"
            rejected = subprocess.run(
                [
                    "python3",
                    str(helper),
                    str(report_path),
                    str(snapshot),
                    str(empty_staging),
                    str(outside_manifest),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(rejected.returncode, 2)
            self.assertIn("escapes the rTorrent snapshot", rejected.stderr)
            self.assertEqual(list(empty_staging.iterdir()), [])

    def test_migration_config_must_match_session_and_keep_database_inside_snapshot(self) -> None:
        helper = ROOT / "scripts" / "validate_migration_config.py"
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            session = base / "torrentng" / "session"
            session.mkdir(parents=True)
            config = base / "config.toml"

            config.write_text(
                f'[daemon]\nsession_dir = "{session}"\n'
                f'[db]\npath = "{base / "outside.db"}"\n',
                encoding="utf-8",
            )
            outside_db = subprocess.run(
                ["python3", str(helper), str(config), str(session)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(outside_db.returncode, 2)
            self.assertIn("db.path must remain inside", outside_db.stderr)

            config.write_text(
                f'[daemon]\nsession_dir = "{session}"\n'
                '[db]\npath = "session.db"\n',
                encoding="utf-8",
            )
            relative_db = subprocess.run(
                ["python3", str(helper), str(config), str(session)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(relative_db.returncode, 0, relative_db.stderr)

            config.write_text(
                f'[daemon]\nsession_dir = "{session}"\n'
                f'[db]\npath = "{session / "state.db"}"\n',
                encoding="utf-8",
            )
            accepted = subprocess.run(
                ["python3", str(helper), str(config), str(session)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(accepted.returncode, 0, accepted.stderr)

    def test_rtorrent_migration_refuses_broad_paths_and_untrusted_api_before_docker(self) -> None:
        script = ROOT / "scripts" / "rtorrent_trusted_complete_migration.sh"
        source = script.read_text(encoding="utf-8")
        self.assertIn('API_TOKEN="${TNG_API_TOKEN:-}"', source)
        self.assertNotIn("--api-token", source)
        self.assertIn('protected_target.py" "$API_URL"', source)
        self.assertIn('validate_migration_config.py" "$TORRENTNGD_CONFIG"', source)
        self.assertIn('mktemp -d -- "$BACKUP_DIR/rtorrent-migration-$STAMP.XXXXXX"', source)

        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            bin_path = base / "bin"
            bin_path.mkdir()
            docker_log = base / "docker.log"
            for name in ("docker", "rsync", "jq", "curl", "torrentngd"):
                executable = bin_path / name
                executable.write_text(
                    "#!/bin/sh\n"
                    'printf "%s\\n" "$*" >> "$DOCKER_LOG"\n'
                    "exit 0\n",
                    encoding="utf-8",
                )
                executable.chmod(0o700)
            compose_file = base / "compose.yml"
            compose_file.write_text("services: {}\n", encoding="utf-8")
            rt_session = base / "rtorrent" / "session"
            rt_config = base / "rtorrent" / "config"
            tng_session = base / "torrentng" / "session"
            for directory in (rt_session, rt_config, tng_session):
                directory.mkdir(parents=True)
            tng_config = base / "torrentng" / "config.toml"
            tng_config.write_text(
                f'[daemon]\nsession_dir = "{tng_session}"\n', encoding="utf-8"
            )

            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "DOCKER_LOG": str(docker_log),
                    "TNG_API_TOKEN": "migration-token-not-for-argv",
                }
            )
            common = [
                "bash",
                str(script),
                "--compose-file",
                str(compose_file),
                "--rtorrent-service",
                "rtorrent",
                "--torrentngd-service",
                "torrentngd",
                "--rtorrent-session-dir",
                str(rt_session),
                "--rtorrent-config-dir",
                str(rt_config),
                "--torrentngd-config",
                str(tng_config),
                "--backup-dir",
                str(base / "backups"),
                "--api-url",
                "http://localhost:18080",
            ]

            broad = subprocess.run(
                common
                + ["--torrentngd-session-dir", "/"],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(broad.returncode, 2)
            self.assertIn("refusing broad TorrentNG session directory", broad.stderr)
            self.assertFalse(docker_log.exists())

            untrusted_environment = environment.copy()
            untrusted_command = common.copy()
            untrusted_command[untrusted_command.index("http://localhost:18080")] = "https://untrusted.example"
            untrusted_command += ["--torrentngd-session-dir", str(tng_session)]
            untrusted = subprocess.run(
                untrusted_command,
                capture_output=True,
                text=True,
                env=untrusted_environment,
            )
            self.assertEqual(untrusted.returncode, 2)
            self.assertIn("refusing protected target", untrusted.stderr)
            self.assertFalse(docker_log.exists())

    def test_rtorrent_migration_failure_restores_session_and_original_service_state(self) -> None:
        script = ROOT / "scripts" / "rtorrent_trusted_complete_migration.sh"
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            bin_path = base / "bin"
            bin_path.mkdir()
            docker_log = base / "docker.jsonl"
            rt_session = base / "rtorrent" / "session"
            rt_config = base / "rtorrent" / "config"
            tng_session = base / "torrentng" / "session"
            for directory in (rt_session / "nested", rt_config, tng_session):
                directory.mkdir(parents=True)
            (rt_session / "nested" / "same.torrent").write_text(
                "torrent fixture", encoding="utf-8"
            )
            (rt_session / "nested" / "same.rtorrent").write_text(
                "resume fixture", encoding="utf-8"
            )
            (rt_config / "rtorrent.rc").write_text("directory = /data\n", encoding="utf-8")
            (tng_session / "state.db").write_text("original state", encoding="utf-8")
            compose_file = base / "compose.yml"
            compose_file.write_text("services: {}\n", encoding="utf-8")
            tng_config = base / "torrentng" / "config.toml"
            tng_config.write_text(
                f'[daemon]\nsession_dir = "{tng_session}"\n', encoding="utf-8"
            )

            stubs = {
                "docker": (
                    "#!/usr/bin/env python3\n"
                    "import json, os, sys\n"
                    "args = sys.argv[1:]\n"
                    "if args and args[0] == 'inspect':\n"
                    "    print('true')\n"
                    "elif args and args[0] == 'compose' and 'ps' in args:\n"
                    "    print('container-' + args[-1])\n"
                    "elif args and args[0] == 'compose':\n"
                    "    with open(os.environ['DOCKER_LOG'], 'a', encoding='utf-8') as output:\n"
                    "        output.write(json.dumps(args) + '\\n')\n"
                ),
                "rsync": (
                    "#!/usr/bin/env python3\n"
                    "import shutil, sys\n"
                    "from pathlib import Path\n"
                    "args = sys.argv[1:]\n"
                    "source = Path(args[-2].rstrip('/'))\n"
                    "destination = Path(args[-1].rstrip('/'))\n"
                    "destination.mkdir(parents=True, exist_ok=True)\n"
                    "if '--delete' in args:\n"
                    "    for entry in destination.iterdir():\n"
                    "        shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()\n"
                    "shutil.copytree(source, destination, dirs_exist_ok=True, copy_function=shutil.copy2, symlinks=True)\n"
                ),
                "jq": (
                    "#!/usr/bin/env python3\n"
                    "import json, sys\n"
                    "if sys.argv[1] == '-r':\n"
                    "    report = json.load(open(sys.argv[-1], encoding='utf-8'))\n"
                    "    for torrent in report['torrents']:\n"
                    "        print(torrent['info_hash'])\n"
                    "elif sys.argv[1] == '-e':\n"
                    "    raise SystemExit(1)\n"
                ),
                "curl": "#!/bin/sh\nprintf '[]\\n'\n",
                "torrentngd": (
                    "#!/usr/bin/env python3\n"
                    "import json, os, sys\n"
                    "from pathlib import Path\n"
                    "args = sys.argv[1:]\n"
                    "if '--apply' in args:\n"
                    "    session = Path(os.environ['TNG_SESSION'])\n"
                    "    (session / 'state.db').write_text('partial import', encoding='utf-8')\n"
                    "    (session / 'partial-import.marker').write_text('partial', encoding='utf-8')\n"
                    "    raise SystemExit(1)\n"
                    "root = Path(args[args.index('--from') + 1])\n"
                    "report = Path(args[args.index('--report-json') + 1])\n"
                    "json.dump({'torrents': [{'info_hash': 'a' * 40, 'torrent_path': str(root / 'nested' / 'same.torrent'), 'resume_path': str(root / 'nested' / 'same.rtorrent')}]}, report.open('w', encoding='utf-8'))\n"
                ),
            }
            for name, contents in stubs.items():
                executable = bin_path / name
                executable.write_text(contents, encoding="utf-8")
                executable.chmod(0o700)

            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "DOCKER_LOG": str(docker_log),
                    "TNG_SESSION": str(tng_session),
                    "TNG_API_TOKEN": "not-visible-in-process-arguments",
                    "RTORRENT_MIGRATION_VERIFY_DELAY_SECS": "0",
                }
            )
            completed = subprocess.run(
                [
                    "bash",
                    str(script),
                    "--compose-file",
                    str(compose_file),
                    "--rtorrent-service",
                    "rtorrent",
                    "--torrentngd-service",
                    "torrentngd",
                    "--rtorrent-session-dir",
                    str(rt_session),
                    "--rtorrent-config-dir",
                    str(rt_config),
                    "--torrentngd-session-dir",
                    str(tng_session),
                    "--torrentngd-config",
                    str(tng_config),
                    "--backup-dir",
                    str(base / "backups"),
                    "--api-url",
                    "http://localhost:18080",
                    "--yes",
                ],
                capture_output=True,
                text=True,
                env=environment,
            )

            self.assertEqual(completed.returncode, 1, completed.stderr)
            self.assertIn("migration failed before commit", completed.stderr)
            self.assertEqual((tng_session / "state.db").read_text(encoding="utf-8"), "original state")
            self.assertFalse((tng_session / "partial-import.marker").exists())
            events = [json.loads(line) for line in docker_log.read_text(encoding="utf-8").splitlines()]
            starts = [event[-1] for event in events if "start" in event]
            self.assertCountEqual(starts, ["rtorrent", "torrentngd"])
            self.assertTrue((rt_session / "nested" / "same.torrent").exists())

    def test_interop_matrix_refuses_nonempty_workdir_without_deleting_it(self) -> None:
        script = ROOT / "scripts" / "interop_matrix.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            bin_path = temp_path / "bin"
            workdir = temp_path / "existing-workdir"
            bin_path.mkdir()
            workdir.mkdir()
            protected_file = workdir / "keep-me.txt"
            protected_file.write_text("user data", encoding="utf-8")
            for name in ("docker", "jq"):
                executable = bin_path / name
                executable.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                executable.chmod(0o700)
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "INTEROP_WORKDIR": str(workdir),
                    "INTEROP_REUSE_STACK": "0",
                }
            )
            completed = subprocess.run(
                ["bash", str(script), "--local"],
                capture_output=True,
                text=True,
                env=environment,
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("refusing to reset non-empty INTEROP_WORKDIR", completed.stderr)
            self.assertEqual(protected_file.read_text(encoding="utf-8"), "user data")

    def test_interop_matrix_refuses_home_and_user_data_workdirs_before_docker(self) -> None:
        script = ROOT / "scripts" / "interop_matrix.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            home = temp_path / "home"
            home.mkdir()
            for index, workdir in enumerate((home, home / "Downloads")):
                workdir.mkdir(exist_ok=True)
                canary = workdir / "downloads" / "qbittorrent" / "public" / f"keep-{index}"
                canary.parent.mkdir(parents=True)
                canary.write_text("user data", encoding="utf-8")
                completed, docker_log = run_interop_preflight(
                    script,
                    temp_path / f"case-{index}",
                    {"HOME": str(home), "INTEROP_WORKDIR": str(workdir)},
                )
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertIn("refusing", completed.stderr)
                self.assertEqual(canary.read_text(encoding="utf-8"), "user data")
                self.assertFalse(docker_log.exists() and docker_log.read_text(encoding="utf-8"))

    def test_interop_matrix_refuses_shared_interop_parent_without_mutating_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            fake_root = temp_path / "repo"
            scripts_dir = fake_root / "scripts"
            workdir = fake_root / "certification" / "interop-runs"
            scripts_dir.mkdir(parents=True)
            workdir.mkdir(parents=True)
            shutil.copy2(ROOT / "scripts" / "interop_matrix.sh", scripts_dir)
            shutil.copy2(ROOT / "scripts" / "curl_policy.sh", scripts_dir)
            canary = workdir / "downloads" / "qbittorrent" / "public" / "keep"
            canary.parent.mkdir(parents=True)
            canary.write_text("shared data", encoding="utf-8")

            completed, docker_log = run_interop_preflight(
                scripts_dir / "interop_matrix.sh",
                temp_path / "case",
                {"HOME": str(temp_path / "home"), "INTEROP_WORKDIR": str(workdir)},
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("shared interop-runs parent", completed.stderr)
            self.assertEqual(canary.read_text(encoding="utf-8"), "shared data")
            self.assertFalse(docker_log.exists() and docker_log.read_text(encoding="utf-8"))

    def test_interop_matrix_requires_explicit_reuse_marker_and_rejects_symlinks(self) -> None:
        script = ROOT / "scripts" / "interop_matrix.sh"
        with tempfile.TemporaryDirectory() as temporary:
            temp_path = Path(temporary)
            unmarked = temp_path / "unmarked"
            unmarked.mkdir()
            canary = unmarked / "keep"
            canary.write_text("user data", encoding="utf-8")
            completed, docker_log = run_interop_preflight(
                script,
                temp_path / "unmarked-run",
                {
                    "HOME": str(temp_path / "home"),
                    "INTEROP_WORKDIR": str(unmarked),
                    "INTEROP_REUSE_STACK": "1",
                },
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("unmarked workdir", completed.stderr)
            self.assertEqual(canary.read_text(encoding="utf-8"), "user data")
            self.assertFalse(docker_log.exists() and docker_log.read_text(encoding="utf-8"))

            reused = temp_path / "reused"
            reused.mkdir()
            (reused / ".tng-interop-workdir").write_text(
                "torrentng-interop-workdir-v1\n", encoding="utf-8"
            )
            outside = temp_path / "outside"
            outside_public = outside / "public"
            outside_public.mkdir(parents=True)
            outside_canary = outside_public / "keep"
            outside_canary.write_text("outside data", encoding="utf-8")
            (reused / "downloads").mkdir()
            (reused / "downloads" / "qbittorrent").symlink_to(outside, target_is_directory=True)
            completed, docker_log = run_interop_preflight(
                script,
                temp_path / "symlink-run",
                {
                    "HOME": str(temp_path / "home"),
                    "INTEROP_WORKDIR": str(reused),
                    "INTEROP_REUSE_STACK": "1",
                    "INTEROP_KEEP_PUBLIC_DATA": "0",
                },
            )
            self.assertEqual(completed.returncode, 2, completed.stderr)
            self.assertIn("containing symlinks", completed.stderr)
            self.assertEqual(outside_canary.read_text(encoding="utf-8"), "outside data")
            self.assertFalse(docker_log.exists() and docker_log.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
