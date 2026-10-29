"""Workflow owner contracts. Run with PyYAML installed, not a firmware toolchain."""
import hashlib
import json
import pathlib
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
OWNERS = {
    'build.yml': ('firmware', ['validate-profile', 'build', 'build-qube', 'build-settings-reset']),
    'ci.yml': ('tests', ['format', 'fetch-check', 'check', 'test', 'discover', 'build', 'bloat-report', 'build-esp']),
}


def load(name):
    return yaml.safe_load((ROOT / '.github/workflows' / name).read_text())


class WorkflowGraphTests(unittest.TestCase):
    def test_every_owner_is_gated_and_reaches_complete(self):
        for name, (profile, owners) in OWNERS.items():
            with self.subTest(workflow=name):
                jobs = load(name)['jobs']
                self.assertIn('plan', jobs)
                for owner in owners:
                    self.assertIn('plan', jobs[owner]['needs'])
                    self.assertIn("needs.plan.outputs.run == 'true'", jobs[owner]['if'])
                self.assertEqual(set(jobs['complete']['needs']), {'plan', *owners})
                self.assertEqual(jobs['complete']['if'], '${{ always() }}')

    def test_original_job_payloads_are_preserved(self):
        # Reviewed owner payload baselines. build.yml additionally enables
        # universal_mac_iso only for approved models and isolates unchanged
        # Qube descriptors; dedicated regressions lock both contracts.
        # Intentional coverage edits must review/update, not drop, this test.
        expected = {
            'build.yml': 'bd0c798ac2529ea4f40944226e506858e5fc844768e34ac7e501de9bddfaefe5',
            'ci.yml': 'e799514a3d8296c6f28a8da50dafe82a5535b44f9b70dc0c43f6b14592fad343',
        }
        for name, (_, owners) in OWNERS.items():
            jobs = load(name)['jobs']
            payload = {job: {k: v for k, v in jobs[job].items() if k not in ('needs', 'if')}
                       for job in owners}
            actual = hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()
            self.assertEqual(actual, expected[name], name)

    def test_event_exclusions_match_completion_contract(self):
        jobs = load('ci.yml')['jobs']
        self.assertEqual(jobs['build-esp']['if'],
                         "needs.plan.outputs.run == 'true' && fromJson(needs.discover.outputs.esp)[0] != null")
        self.assertEqual(jobs['bloat-report']['if'],
                         "needs.plan.outputs.run == 'true' && github.event_name == 'pull_request'")
        self.assertEqual(jobs['build']['needs'], ['plan', 'discover'])
        self.assertEqual(jobs['bloat-report']['needs'], ['plan', 'build'])

    def test_automatic_push_is_production_only(self):
        for name in ('build.yml', 'ci.yml'):
            text = load(name)
            triggers = text.get('on', text.get(True))
            self.assertEqual(triggers['push']['branches'], ['main'])

    def test_plan_permissions_outputs_and_proof_conditions(self):
        for name, (profile, _) in OWNERS.items():
            workflow = load(name)
            jobs = workflow['jobs']
            self.assertIn('github.event_name', workflow['concurrency']['group'])
            self.assertEqual(jobs['plan']['permissions'],
                             {'contents': 'read', 'actions': 'read', 'pull-requests': 'read'})
            self.assertEqual(jobs['plan']['steps'][0]['with']['fetch-depth'], 0)
            self.assertEqual(set(jobs['plan']['outputs']), {'run', 'reason', 'source_run'})
            policy = jobs['plan']['steps'][-1]
            self.assertEqual(policy['env'], {'GH_TOKEN': '${{ secrets.GITHUB_TOKEN }}'})
            self.assertEqual(policy['run'], f'python3 .github/scripts/ci_policy.py plan --profile {profile}')
            complete = jobs['complete']
            self.assertEqual(complete['permissions'], {'contents': 'read'})
            self.assertEqual(complete['steps'][1]['env'], {'CI_NEEDS': '${{ toJSON(needs) }}'})
            for step in complete['steps'][2:]:
                self.assertEqual(step['if'], "steps.verify.outputs.full == 'true'")
            upload = complete['steps'][-1]
            self.assertEqual(upload['uses'], 'actions/upload-artifact@v4')
            self.assertEqual(upload['with']['name'],
                             f'ci-proof-{profile}-${{{{ github.run_id }}}}-${{{{ github.run_attempt }}}}')
            self.assertEqual(upload['with']['if-no-files-found'], 'error')


if __name__ == '__main__':
    unittest.main()
