from __future__ import annotations

import unittest
from unittest.mock import patch

from scripts.protected_target import ALLOWLIST_ENV, validate_protected_target


class ProtectedTargetTests(unittest.TestCase):
    def test_loopback_http_targets_are_allowed_without_remote_allowlist(self) -> None:
        with patch.dict("os.environ", {ALLOWLIST_ENV: ""}):
            self.assertEqual(
                validate_protected_target("http://localhost:28080/"),
                "http://localhost:28080",
            )
            self.assertEqual(
                validate_protected_target("http://127.0.0.1:28080"),
                "http://127.0.0.1:28080",
            )
            self.assertEqual(
                validate_protected_target("http://[::1]:28080"),
                "http://[::1]:28080",
            )

    def test_allowlisted_remote_https_origin_is_exact(self) -> None:
        with patch.dict("os.environ", {ALLOWLIST_ENV: "https://load.example:443"}):
            self.assertEqual(
                validate_protected_target("https://load.example/api"),
                "https://load.example/api",
            )
            with self.assertRaisesRegex(ValueError, "not listed"):
                validate_protected_target("https://load.example:8443")
            with self.assertRaisesRegex(ValueError, "not listed"):
                validate_protected_target("https://other.example")

    def test_remote_http_is_rejected_even_if_allowlisted(self) -> None:
        with self.assertRaisesRegex(ValueError, "must use HTTPS"):
            validate_protected_target(
                "http://load.example:8080", "http://load.example:8080"
            )

    def test_private_http_exception_is_exact_and_requires_a_private_ip(self) -> None:
        self.assertEqual(
            validate_protected_target(
                "http://10.244.100.2:8080",
                allow_private_http_origins="http://10.244.100.2:8080",
            ),
            "http://10.244.100.2:8080",
        )
        for url in ("http://10.244.100.3:8080", "http://192.0.2.4:8080"):
            with self.subTest(url=url), self.assertRaisesRegex(ValueError, "must use HTTPS"):
                validate_protected_target(
                    url, allow_private_http_origins="http://10.244.100.2:8080"
                )

    def test_private_http_exception_accepts_only_rfc1918_ula_and_mapped_rfc1918(self) -> None:
        accepted = (
            "http://10.0.0.1",
            "http://172.31.255.254",
            "http://192.168.1.1",
            "http://[fc00::1]",
            "http://[fd12:3456::1]",
            "http://[::ffff:10.0.0.1]",
        )
        for url in accepted:
            with self.subTest(url=url):
                self.assertEqual(
                    validate_protected_target(url, allow_private_http_origins=url),
                    url,
                )

    def test_private_http_exception_rejects_non_private_special_use_ranges(self) -> None:
        rejected = (
            "http://0.1.2.3",
            "http://192.0.0.8",
            "http://198.18.0.1",
            "http://198.51.100.1",
            "http://203.0.113.1",
            "http://[2001:db8::1]",
            "http://[::ffff:198.51.100.1]",
        )
        for url in rejected:
            with self.subTest(url=url), self.assertRaisesRegex(ValueError, "must use HTTPS"):
                validate_protected_target(url, allow_private_http_origins=url)

    def test_rejects_userinfo_query_fragment_and_ambiguous_urls(self) -> None:
        invalid_urls = (
            "https://user:password@load.example",
            "https://user@load.example",
            "https://load.example?token=secret",
            "https://load.example#fragment",
            "https://load.example/with\\backslash",
            " https://load.example",
            "https://load.example:99999",
            "ftp://load.example",
        )
        for url in invalid_urls:
            with self.subTest(url=url), self.assertRaises(ValueError):
                validate_protected_target(url, "https://load.example")

    def test_invalid_allowlist_fails_closed(self) -> None:
        with self.assertRaisesRegex(ValueError, "only an origin"):
            validate_protected_target(
                "https://load.example", "https://load.example/path"
            )
        with self.assertRaisesRegex(ValueError, "must use HTTPS"):
            validate_protected_target(
                "https://load.example", "http://other.example"
            )


if __name__ == "__main__":
    unittest.main()
