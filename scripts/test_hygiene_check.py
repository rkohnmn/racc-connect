"""Regression tests for exact history hygiene exemptions."""
import unittest

from hygiene_check import (
    HISTORY_BASELINE, content_findings, is_baselined_history_finding,
    unreviewed_history_findings,
)


class HygieneBaselineTests(unittest.TestCase):
    def test_only_the_two_reviewed_profile_path_blobs_are_baselined(self):
        self.assertEqual(
            HISTORY_BASELINE,
            frozenset({
                ("c0de3017b472cbcc654d6803669def349d8e9e79", "docs/TRANSPORT_TRACE_M2_5.txt", "personal-windows-profile-path"),
                ("70b87fdf00740cd18eecb50bf71dfe2a0389f332", "docs/TRANSPORT_TRACE_M2_5.txt", "personal-windows-profile-path"),
            }),
        )
        for object_id, path, rule in HISTORY_BASELINE:
            with self.subTest(object_id=object_id[:12]):
                self.assertTrue(is_baselined_history_finding(object_id, path, rule))

    def test_only_exact_history_blob_path_rule_matches_baseline(self):
        known_id, known_path, known_rule = next(iter(HISTORY_BASELINE))
        cases = [
            ("f" * 40, known_path, known_rule),
            (known_id, "docs/another-file.txt", known_rule),
            (known_id, known_path, "github-token"),
        ]
        for object_id, path, rule in cases:
            with self.subTest(object_id=object_id[:12], path=path, rule=rule):
                self.assertFalse(is_baselined_history_finding(object_id, path, rule))

    def test_new_history_blob_fails_while_exact_old_blobs_are_ignored(self):
        # Assemble a profile-shaped path so the test source itself does not
        # contain a scan match.
        slash = bytes([92])
        sample = b"C:" + slash + b"Users" + slash + b"ReviewedProfile" + slash + b"fixture.txt"
        self.assertTrue(any("personal-windows-profile-path" in item for item in content_findings(sample, "fixture")))
        for object_id, path, rule in HISTORY_BASELINE:
            with self.subTest(object_id=object_id[:12]):
                self.assertEqual([], unreviewed_history_findings(object_id, path, sample))
        new_blob = "f" * 40
        self.assertEqual(
            [f"history blob {new_blob[:12]} docs/TRANSPORT_TRACE_M2_5.txt: personal-windows-profile-path"],
            unreviewed_history_findings(new_blob, "docs/TRANSPORT_TRACE_M2_5.txt", sample),
        )

    def test_worktree_finding_is_never_exempted(self):
        _, known_path, _ = next(iter(HISTORY_BASELINE))
        slash = bytes([92])
        sample = b"C:" + slash + b"Users" + slash + b"ReviewedProfile" + slash + b"fixture.txt"
        findings = content_findings(sample, f"working tree {known_path}")
        self.assertEqual(1, len(findings))
        self.assertIn("personal-windows-profile-path", findings[0])


if __name__ == "__main__":
    unittest.main()
