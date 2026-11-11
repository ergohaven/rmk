"""Workflow owner contracts. Run with PyYAML installed, not a firmware toolchain."""
import hashlib
import json
import pathlib
import os
import re
import subprocess
import tempfile
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
OWNERS = {
    'build.yml': ('firmware', ['validate-profile', 'build', 'build-qube', 'build-settings-reset']),
    'ci.yml': ('tests', ['format', 'fetch-check', 'check', 'test', 'host', 'qemu-rynk', 'discover', 'build',
                         'bloat-report', 'build-esp']),
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
        # Pre-policy owner payloads from 89a6f853; build.yml since gained the Phenom
        # rows, selectable Vial-only rows and isolated Qube descriptors; ci.yml keeps
        # the target QEMU/LLVM runner dependencies and step-local builtin ESP token.
        # Intentional future
        # coverage edits must review/update these baselines, not drop this test.
        # build.yml includes the Qube display settings/storage regression step
        # and K:04/Micro Rynk ANSI/ISO definitions and feature selection.
        expected = {
            'build.yml': 'b7d93136eb28a7334894dab373ae0625f44ba39e07574414abf8f956387a028b',
            'ci.yml': '911607762e45264261657e42913c164ee41c4a5ae1f6cd4c157c9e4641070bab',
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

    def test_grouped_discovery_matches_example_build_owner(self):
        # Execute the real producer, without compiling or downloading anything.
        with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as temp:
            output = pathlib.Path(temp) / 'matrix'
            result = subprocess.run(
                ['bash', '.github/ci/discover.sh'], cwd=ROOT,
                env={**os.environ, 'GITHUB_OUTPUT': str(output)},
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            matrices = dict(line.split('=', 1) for line in output.read_text().splitlines())
            groups = json.loads(matrices['stable'])
        self.assertTrue(groups)
        build = load('ci.yml')['jobs']['build']
        consumed = set(re.findall(r'matrix\.([a-z_]+)', json.dumps(build)))
        examples = []
        for group in groups:
            self.assertTrue(consumed <= group.keys(),
                            f"{group['name']}: missing {sorted(consumed - group.keys())}")
            self.assertTrue(group['examples'])
            self.assertEqual(group['arm_gcc'],
                             any('nrf' in example['dir'] for example in group['examples']))
            self.assertTrue(all(example['target'] == group['target']
                                for example in group['examples']))
            examples.extend(example['dir'] for example in group['examples'])
        esp = json.loads(matrices['esp'])
        self.assertEqual(len(examples), len(set(examples)))
        inventory = subprocess.run(
            ['bash', '-c', 'source .github/ci/_lib.sh; '
             'while IFS= read -r manifest; do '
             'target="$(get_example_target "$manifest")"; '
             'if [[ -n "$target" ]]; then printf "%s\\n" "$manifest"; fi; '
             'done < <(list_example_manifests)'],
            cwd=ROOT, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(inventory.returncode, 0, inventory.stderr)
        expected = [str(pathlib.PurePosixPath(path).parent)
                    for path in inventory.stdout.splitlines()]
        self.assertCountEqual(examples + [example['dir'] for example in esp], expected)
        calls = [step for step in build['steps']
                 if step.get('run') == '.github/ci/build-examples.sh']
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]['env'], {
            'EXAMPLES': '${{ toJson(matrix.examples) }}',
            'TARGET': '${{ matrix.target }}',
            'COMPARE_BASE': "${{ github.event_name == 'pull_request' && matrix.bloat }}",
        })
        self.assertFalse(any('matrix.dir' in step.get('working-directory', '')
                             for step in build['steps']))

    def test_pull_requests_cover_production_and_integration(self):
        workflow = load('ci.yml')
        triggers = workflow.get('on', workflow.get(True))
        self.assertEqual(triggers['pull_request']['branches'], ['main', 'upstream-merge'])
        self.assertIn('workflow_dispatch', triggers)

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
