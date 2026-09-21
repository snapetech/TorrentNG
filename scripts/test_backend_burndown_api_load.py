from __future__ import annotations

import contextlib
import io
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request

from scripts import backend_burndown_api_load as load


class ApiLoadSecurityTests(unittest.TestCase):
    def test_opener_bypasses_ambient_proxies_and_refuses_redirects(self) -> None:
        self.assertFalse(any(isinstance(handler, ProxyHandler) for handler in load.HTTP.handlers))

        handler = load.NoRedirectHandler()
        request = Request("https://load.example/health")
        self.assertIsNone(handler.redirect_request(request, None, 302, "Found", {}, "https://attacker.example/"))

    def test_direct_http_ignores_proxy_and_does_not_follow_redirects(self) -> None:
        observed_authorizations: list[str | None] = []
        redirected_requests: list[str] = []
        redirect_target = ThreadingHTTPServer(
            ("127.0.0.1", 0),
            type(
                "RedirectTargetHandler",
                (BaseHTTPRequestHandler,),
                {
                    "do_GET": lambda self: redirected_requests.append(
                        self.headers.get("Authorization", "")
                    ),
                    "log_message": lambda self, *args: None,
                },
            ),
        )

        class LocalHandler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                observed_authorizations.append(self.headers.get("Authorization"))
                if self.path == "/redirect":
                    self.send_response(302)
                    self.send_header(
                        "Location",
                        f"http://127.0.0.1:{redirect_target.server_port}/capture",
                    )
                    self.end_headers()
                else:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"ok")

            def log_message(self, *args) -> None:
                return

        local_server = ThreadingHTTPServer(("127.0.0.1", 0), LocalHandler)
        redirect_thread = threading.Thread(target=redirect_target.serve_forever, daemon=True)
        local_thread = threading.Thread(target=local_server.serve_forever, daemon=True)
        redirect_thread.start()
        local_thread.start()
        try:
            with patch.dict(
                "os.environ",
                {
                    "HTTP_PROXY": "http://127.0.0.1:1",
                    "http_proxy": "http://127.0.0.1:1",
                    "NO_PROXY": "",
                    "no_proxy": "",
                },
            ):
                response = load.open_direct(
                    Request(
                        f"http://127.0.0.1:{local_server.server_port}/ok",
                        headers={"Authorization": "Bearer test-token"},
                    ),
                    2.0,
                )
                self.assertEqual(response.read(), b"ok")
                response.close()
                self.assertEqual(observed_authorizations, ["Bearer test-token"])

                with self.assertRaises(HTTPError) as error:
                    load.open_direct(
                        Request(
                            f"http://127.0.0.1:{local_server.server_port}/redirect",
                            headers={"Authorization": "Bearer test-token"},
                        ),
                        2.0,
                    )
                self.assertEqual(error.exception.code, 302)
                error.exception.close()
                self.assertEqual(redirected_requests, [])
        finally:
            local_server.shutdown()
            redirect_target.shutdown()
            local_server.server_close()
            redirect_target.server_close()
            local_thread.join(timeout=2.0)
            redirect_thread.join(timeout=2.0)

    def test_worker_and_duration_inputs_are_bounded(self) -> None:
        with patch.dict("os.environ", {"TNG_LOAD_CLIENTS": "65"}):
            with self.assertRaisesRegex(ValueError, "between 1 and 64"):
                load.env_int("TNG_LOAD_CLIENTS", 32, 1, 64)

        with patch.dict("os.environ", {"TNG_LOAD_DURATION_SECONDS": "inf"}):
            with self.assertRaisesRegex(ValueError, "finite"):
                load.env_float("TNG_LOAD_DURATION_SECONDS", 30.0, 0.1, 1200.0)

    def test_unapproved_remote_target_fails_before_any_request(self) -> None:
        environment = {
            "TNG_BASE_URL": "https://attacker.example",
            "TNG_BACKEND_API_ALLOWED_ORIGINS": "",
        }
        stderr = io.StringIO()
        with (
            patch.dict("os.environ", environment, clear=False),
            patch.object(sys, "argv", ["backend_burndown_api_load.py", "unused-report.md"]),
            contextlib.redirect_stderr(stderr),
        ):
            result = load.main()
        self.assertEqual(result, 2)
        self.assertIn("not listed", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
