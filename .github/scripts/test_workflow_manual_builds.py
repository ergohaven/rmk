"""Exercise the real planner CLI without compiling product code."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ManualBuildTests(unittest.TestCase):
    def plan(self, event_name, ref, *, manual_builds=True, profile='tests', event=None):
        with tempfile.TemporaryDirectory() as tmp:
            payload = Path(tmp) / 'event.json'
            payload.write_text(json.dumps(event or {}))
            command = ['python3', str(ROOT / '.github/scripts/ci_policy.py'),
                       'plan', '--profile', profile]
            if manual_builds:
                command.append('--manual-builds')
            result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True,
                                    env={**os.environ, 'GITHUB_EVENT_NAME': event_name,
                                         'GITHUB_REF': ref, 'GITHUB_EVENT_PATH': str(payload),
                                         'GITHUB_OUTPUT': '', 'GITHUB_STEP_SUMMARY': ''})
            self.assertEqual(result.returncode, 0, result.stderr)
            return dict(line.split('=', 1) for line in result.stdout.splitlines())

    def test_automatic_product_pr_defers_heavy_validation_without_diff_or_api(self):
        # Deliberately unavailable diff objects: automatic selection must not need them.
        event = {'pull_request': {'base': {'sha': 'a' * 40}, 'head': {'sha': 'b' * 40}}}
        for profile in ('tests', 'firmware'):
            with self.subTest(profile=profile):
                result = self.plan('pull_request', 'refs/pull/93/merge', profile=profile, event=event)
                self.assertEqual(result, {'run': 'false',
                                         'reason': 'manual-build-request-required', 'source_run': ''})

    def test_automatic_main_push_defers_heavy_validation(self):
        for profile in ('tests', 'firmware'):
            with self.subTest(profile=profile):
                result = self.plan('push', 'refs/heads/main', profile=profile)
                self.assertEqual(result['run'], 'false')
                self.assertEqual(result['reason'], 'manual-build-request-required')
                self.assertEqual(result['source_run'], '')

    def test_explicit_dispatch_still_selects_full_coverage(self):
        for profile in ('tests', 'firmware'):
            with self.subTest(profile=profile):
                result = self.plan('workflow_dispatch', 'refs/heads/main', profile=profile)
                self.assertEqual(result['run'], 'true')
                self.assertEqual(result['reason'], 'event-requires-full-validation')

    def test_release_tag_and_unknown_events_are_not_suppressed(self):
        for event_name, ref in [('push', 'refs/tags/v1'), ('release', 'refs/tags/v1'),
                                ('workflow_call', 'refs/heads/main'), ('unknown', 'refs/heads/main'),
                                ('push', 'unknown'), ('pull_request', 'unknown')]:
            with self.subTest(event=event_name, ref=ref):
                self.assertEqual(self.plan(event_name, ref)['run'], 'true')

    def test_unflagged_validation_is_unchanged(self):
        result = self.plan('pull_request', 'refs/pull/93/merge', manual_builds=False)
        self.assertEqual(result['run'], 'true')
        self.assertNotEqual(result['reason'], 'manual-build-request-required')


if __name__ == '__main__':
    unittest.main()
