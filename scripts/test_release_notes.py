import unittest

from scripts.release_notes import parse_release_note


def release_note(body: str, action: str = "none") -> dict:
    content = f"""---
category: fixed
audience: users, operators
area: storage
action: {action}
breaking: false
---
{body}
"""
    return parse_release_note("release-notes/test.md", content)


class ReleaseNoteValidationTests(unittest.TestCase):
    def test_accepts_valid_note(self) -> None:
        note = release_note(
            "Torrent recovery now resumes from the saved log after the data path changes."
        )

        self.assertEqual(note["errors"], [])

    def test_accepts_brand_styled_rtorrent_at_sentence_start(self) -> None:
        note = release_note(
            "rTorrent log recovery is now recorded consistently when a previously missing log file appears."
        )

        self.assertEqual(note["errors"], [])

    def test_rejects_html_comment_delimiters_in_action(self) -> None:
        for delimiter in ("<!--", "-->"):
            with self.subTest(delimiter=delimiter):
                note = release_note(
                    "Torrent recovery now resumes from the saved log after the data path changes.",
                    f"restart the service {delimiter}",
                )

                self.assertIn(
                    "action contains a placeholder or HTML comment", note["errors"]
                )

    def test_rejects_html_comment_delimiters_in_body(self) -> None:
        for delimiter in ("<!--", "-->"):
            with self.subTest(delimiter=delimiter):
                note = release_note(
                    f"Torrent recovery now resumes from the saved log {delimiter} after the data path changes."
                )

                self.assertIn(
                    "body contains a placeholder or HTML comment", note["errors"]
                )

    def test_rejects_placeholders_case_insensitively(self) -> None:
        action_note = release_note(
            "Torrent recovery now resumes from the saved log after the data path changes.",
            "Complete the TODO item",
        )
        body_note = release_note(
            "Torrent recovery now resumes from the saved log after the TODO is complete."
        )

        self.assertIn(
            "action contains a placeholder or HTML comment", action_note["errors"]
        )
        self.assertIn("body contains a placeholder or HTML comment", body_note["errors"])


if __name__ == "__main__":
    unittest.main()
