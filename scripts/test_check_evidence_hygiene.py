#!/usr/bin/env python3
"""Contract tests for scripts/check_evidence_hygiene.py (027-FR-016, 027-SC-004)."""

from __future__ import annotations

import importlib.util
import random
import string
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check_evidence_hygiene.py")
_SPEC = importlib.util.spec_from_file_location("check_evidence_hygiene", SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
hygiene = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(hygiene)

PLANTED = {
    "email": "owner: someone@gmail.com",
    "invite code": 'invite_code = "ABCD1234"',
    "credential": "Authorization: Bearer abcdefghijklmnop",
    "local path": "saved to /home/max/Code/x.png",
    "content hash": "a" * 64,
}
CLEAN = (
    "correlation_id: 7f3c2d1e-0000-4000-8000-000000000001\n"
    "elapsed_ms: 42\ncount: 3\ncommit: " + "b" * 40 + "\n"
)


class EvidenceHygieneTest(unittest.TestCase):
    def test_027_FR_016_each_planted_violation_is_reported(self) -> None:
        for rule, text in PLANTED.items():
            with self.subTest(rule=rule):
                self.assertIn(rule, [n for _, n in hygiene.scan_text(text)])

    def test_027_SC_004_clean_evidence_passes(self) -> None:
        self.assertEqual(hygiene.scan_text(CLEAN), [])

    def test_027_FR_016_hash_allowed_only_under_identity_keys(self) -> None:
        digest = "c" * 64
        for key in hygiene.ALLOWED_HASH_KEYS:
            self.assertEqual(hygiene.scan_text(f'  "{key}": "{digest}",'), [])
        flagged = hygiene.scan_text(f'  "other_key": "{digest}",')
        self.assertIn("content hash", [n for _, n in flagged])

    def test_027_FR_016_reserved_example_email_allowed_real_domain_not(self) -> None:
        self.assertEqual(hygiene.scan_text("max@example.com"), [])
        self.assertTrue(hygiene.scan_text("max@gmail.com"))
        self.assertTrue(hygiene.scan_text("max@notexample.com"))

    def test_027_FR_016_example_domain_exemption_is_end_anchored(self) -> None:
        for text in (
            "a@example.com.evil.org",
            "a@example.org.attacker.io",
            "a@example.com-evil.org",
            "a@host.test.evil.org",
        ):
            with self.subTest(text=text):
                self.assertIn("email", [n for _, n in hygiene.scan_text(text)])
        for text in ("a@example.com", "a@example.com.", "(a@mail.example.net)"):
            with self.subTest(text=text):
                self.assertEqual(hygiene.scan_text(text), [])

    def test_027_FR_016_realistic_token_urlsafe_values_are_caught(self) -> None:
        # Shape of secrets.token_urlsafe(32): 43 base64url characters.
        tok = "T86YNV6qPH05-B60vx6qbuPfYF8mmjXCKm3QNtZ8p38"
        self.assertEqual(len(tok), 43)
        for text in (
            tok,
            f'  "value": "{tok}"',
            f'  "token": "{tok}"',
            f'  "access_token": "{tok}"',
            f'  "refresh_token": "{tok}"',
            f'  "code": "{tok}"',
            f"Cookie: brainbuddy_session={tok}",
            f"brainbuddy_session={tok}",
            "FLY_API_TOKEN=FlyV1 fm2_lJPECAAAAAAAAB0QxBA",
            'password: "p@ss!word1"',
            "password=!?#$%^&*()abc",
        ):
            with self.subTest(text=text):
                self.assertTrue(hygiene.scan_text(text), text)
        rng = random.Random(27)
        alphabet = string.ascii_letters + string.digits + "-_"
        values = ["".join(rng.choice(alphabet) for _ in range(43)) for _ in range(500)]
        missed = [v for v in values if not hygiene.scan_text(f'"x": "{v}"')]
        # Only a token with no digit and no "-"/"_" (about 1 in 7,500) is allowed
        # through, to keep camelCase test names from firing.
        self.assertTrue(all(v.isalpha() for v in missed), missed)
        self.assertLessEqual(len(missed), 2)

    def test_027_SC_004_ids_and_error_codes_are_not_secrets(self) -> None:
        for text in (
            '"session_id": "review_3e8f1a6c-5d2b-4c97-b04e-9a7d2f6c1e85"',
            '"form_id": "form_3E8F1A6C-5D2B-4C97-B04E-9A7D2F6C1E85"',
            '"code": "validation_error"',
            '"token_count": 12345678',
            "a token: cleanTitle indexes a UTF-16 mask",
            'password: "<redacted>"',
            "test_027_FR_016_each_planted_violation_is_reported_in_full",
            "commit: " + "b" * 40,
        ):
            with self.subTest(text=text):
                self.assertEqual(hygiene.scan_text(text), [], text)

    def test_scan_roots_walks_files_and_skips_binary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "ok.md").write_text(CLEAN, encoding="utf-8")
            (root / "tone.wav").write_bytes(b"\xff\xfe\x00\x80" * 8)
            self.assertEqual(hygiene.scan_roots([root]), [])
            (root / "bad.json").write_text(PLANTED["email"], encoding="utf-8")
            problems = hygiene.scan_roots([root])
            self.assertEqual(len(problems), 1)
            self.assertIn("bad.json:1: email", problems[0])

    def test_main_exit_codes_and_missing_roots(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(hygiene.main([str(Path(tmp) / "absent")]), 0)
            (Path(tmp) / "x.txt").write_text(PLANTED["local path"], encoding="utf-8")
            self.assertEqual(hygiene.main([tmp]), 1)

    def test_current_tree_passes(self) -> None:
        repo = SCRIPT.parent.parent
        roots = [repo / r for r in hygiene.DEFAULT_ROOTS]
        self.assertEqual(hygiene.scan_roots([r for r in roots if r.exists()]), [])


if __name__ == "__main__":
    unittest.main()
