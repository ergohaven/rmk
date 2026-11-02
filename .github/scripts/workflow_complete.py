"""Fail closed on incomplete workflow graphs; never mint proof for a skipped plan."""
import json
import os
import sys

OWNERS = {
    'firmware': {'validate-profile', 'build', 'build-qube', 'build-settings-reset'},
    'tests': {'format', 'fetch-check', 'check', 'test', 'host', 'qemu-rynk', 'discover', 'build', 'build-esp',
              'bloat-report'},
}


def verify(profile, needs, event):
    owners = OWNERS[profile]
    if set(needs) != owners | {'plan'}:
        raise ValueError('Completion dependencies do not match the full owning graph')
    plan = needs['plan']
    if plan['result'] != 'success':
        raise ValueError('Planner did not succeed')
    run = plan.get('outputs', {}).get('run')
    if run not in ('true', 'false'):
        raise ValueError('Planner did not return a valid run decision')
    if run == 'false':
        if any(needs[j]['result'] != 'skipped' for j in owners):
            raise ValueError('Skipped plan unexpectedly executed owning jobs')
        return False
    excluded = set()
    if profile == 'tests':
        if event != 'pull_request':
            excluded.add('bloat-report')
        # Exact counterpart of fromJson(discover.outputs.esp)[0] != null.
        # Invalid/missing discover output is an error, never an empty ESP matrix.
        esp = json.loads(needs['discover']['outputs']['esp'])
        if not isinstance(esp, list):
            raise ValueError('ESP discovery output is not an array')
        if not esp or esp[0] is None:
            excluded.add('build-esp')
    for owner in sorted(owners):
        expected = 'skipped' if owner in excluded else 'success'
        if needs[owner]['result'] != expected:
            raise ValueError(f"{owner}: expected {expected}, got {needs[owner]['result']}")
    return True


def main():
    profile = sys.argv[1]
    needs = json.loads(os.environ['CI_NEEDS'])
    full = verify(profile, needs, os.environ['GITHUB_EVENT_NAME'])
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write(f'full={str(full).lower()}\n')
    outputs = needs['plan'].get('outputs', {})
    with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
        summary.write(f'## {profile} coverage\n\n')
        summary.write('Full owning graph succeeded; eligible to record proof.\n' if full else
                      'No new full coverage; skipped/reused according to the policy planner.\n')
        # JSON quoting keeps planner metadata readable without shell interpolation.
        summary.write('\n```json\n' + json.dumps(outputs, indent=2) + '\n```\n')


if __name__ == '__main__':
    main()
