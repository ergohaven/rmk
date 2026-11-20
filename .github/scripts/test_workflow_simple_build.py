"""Ordinary firmware dispatch is not a release-qualification run."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
RELEASE = "${{ inputs.full-validation == true || (github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')) }}"
ARTIFACT = "github.event_name == 'workflow_dispatch' || inputs.full-validation == true || (github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v'))"


def workflow(name):
    return yaml.safe_load((ROOT / '.github/workflows' / name).read_text())


class SimpleBuildTests(unittest.TestCase):
    def test_manual_build_has_only_the_two_existing_keyboard_matrices(self):
        jobs = workflow('build.yml')['jobs']
        self.assertEqual(jobs['plan']['if'], "github.event_name != 'workflow_dispatch'")
        self.assertEqual(jobs['complete']['if'], "${{ always() && github.event_name != 'workflow_dispatch' }}")
        for owner in ('build', 'build-qube'):
            job = jobs[owner]
            self.assertNotIn('needs', job)
            self.assertEqual(job['if'], ARTIFACT)
            self.assertNotIn('container', job)
            for step in job['steps']:
                self.assertFalse(any(command in step.get('run', '') for command in
                                     ('cargo test', 'nextest', 'ci_policy.py', 'docker ', 'nix ')))
                self.assertNotIn('secrets.', json.dumps(step))
        for owner in ('validate-profile', 'build-settings-reset'):
            self.assertEqual(jobs[owner]['if'], "needs.plan.outputs.run == 'true'")
            self.assertEqual(jobs[owner]['needs'], ['plan'])

    def test_release_call_and_tag_are_explicit_and_automatic_push_stays_cheap(self):
        for name in ('build.yml', 'ci.yml'):
            data = workflow(name)
            events = data.get('on', data.get(True))
            self.assertEqual(events['push'], {'branches': ['main'], 'tags': ['v*']})
            self.assertEqual(events['workflow_call']['inputs']['full-validation']['default'], True)
            self.assertEqual(data['jobs']['plan']['steps'][-1]['env'], {'FULL_VALIDATION': RELEASE})
            self.assertEqual(data['permissions'], {'contents': 'read'})
        events = workflow('ci.yml')
        self.assertNotIn('workflow_dispatch', events.get('on', events.get(True)))

    def test_real_release_selector_writes_both_boolean_values_without_network(self):
        for name in ('build.yml', 'ci.yml'):
            body = workflow(name)['jobs']['plan']['steps'][-1]['run']
            with tempfile.TemporaryDirectory() as tmp:
                for value in ('true', 'false'):
                    target = Path(tmp) / value
                    result = subprocess.run(['bash', '-euo', 'pipefail', '-c', body],
                                            env={**os.environ, 'FULL_VALIDATION': value,
                                                 'GITHUB_OUTPUT': str(target)}, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(target.read_text(), f'run={value}\n')

    def test_reusable_owners_cannot_cancel_each_other_in_the_publish_caller(self):
        groups = [workflow(name)['concurrency']['group'] for name in ('build.yml', 'ci.yml')]
        self.assertNotEqual(*groups)
        self.assertTrue(all('github.workflow' not in group and 'github.event_name' in group for group in groups))

    def test_publication_waits_for_complete_tests_and_firmware_without_new_secrets(self):
        current = workflow('publish.yml')
        original = yaml.safe_load(subprocess.check_output(
            ['git', 'show', '41e39aefcf11fd954ef7a3ccbf3a7ac07cec835c:.github/workflows/publish.yml'],
            cwd=ROOT, text=True))
        jobs = current['jobs']
        self.assertEqual(jobs['publish']['needs'], ['plan', 'validate-release', 'validate-firmware'])
        for name, path in (('validate-release', 'ci.yml'), ('validate-firmware', 'build.yml')):
            job = jobs[name]
            self.assertEqual(job['uses'], './.github/workflows/' + path)
            self.assertEqual(job['needs'], 'plan')
            self.assertEqual(job['if'], "needs.plan.outputs.release == 'true'")
            self.assertEqual(job['with'], {'full-validation': True})
            self.assertNotIn('secrets', job)
        restored = copy.deepcopy(current)
        restored['jobs'].pop('validate-release')
        restored['jobs'].pop('validate-firmware')
        restored['jobs']['publish']['needs'] = 'plan'
        self.assertEqual(restored, original)


if __name__ == '__main__':
    unittest.main()
