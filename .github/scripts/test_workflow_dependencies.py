"""Ubuntu runners must provide the tools selected by each CI owner."""
import pathlib
import json
import os
import subprocess
import tempfile
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]


class RunnerDependencyTests(unittest.TestCase):
    def jobs(self):
        return yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']

    def test_qemu_uses_the_ubuntu_riscv_provider(self):
        self.assertEqual(self.jobs()['qemu-rynk']['runs-on'], 'ubuntu-24.04')
        steps = self.jobs()['qemu-rynk']['steps']
        install = next(step['run'] for step in steps if step.get('name') == 'Install QEMU')
        self.assertIn('qemu-system-misc', install)
        self.assertNotIn('install -y qemu-system-riscv ', install)
        self.assertIn('qemu-system-riscv32 --version', install)

    def test_espup_uses_only_builtin_token_at_install_step(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        job = workflow['jobs']['build-esp']
        install = next(step for step in job['steps']
                       if step.get('name') == 'Install espup and esp toolchain')
        self.assertEqual(install.get('env'), {'GITHUB_TOKEN': '${{ secrets.GITHUB_TOKEN }}'})
        self.assertEqual(install['run'], 'cargo binstall --force espup -y\nespup install\n')
        # Do not expose the token to example builds or broaden permissions.
        self.assertNotIn('GITHUB_TOKEN', workflow.get('env', {}))
        self.assertNotIn('GITHUB_TOKEN', job.get('env', {}))
        for step in job['steps']:
            if step is not install:
                self.assertNotIn('GITHUB_TOKEN', step.get('env', {}))
        self.assertNotIn('permissions', job)

    def test_nordic_bindgen_uses_installed_distro_llvm(self):
        job = self.jobs()['build']
        self.assertNotIn('LIBCLANG_PATH', job.get('env', {}))
        self.assertNotIn('CLANG_PATH', job.get('env', {}))
        install = next(step for step in job['steps'] if step.get('name') == 'Install ARM build dependencies')
        # Discovery now groups examples; arm_gcc means any member needs Nordic tools.
        self.assertEqual(install['if'], 'matrix.arm_gcc')
        for package in ('gcc-arm-none-eabi', 'clang', 'libclang-dev', 'llvm-dev'):
            self.assertIn(package, install['run'])
        self.assertIn('/usr/bin/llvm-config --libdir', install['run'])
        self.assertIn('CLANG_PATH=/usr/bin/clang', install['run'])
        self.assertIn('LIBCLANG_PATH=%s', install['run'])
        self.assertIn('GITHUB_ENV', install['run'])
        self.assertIn('test -e "$libclang_dir/libclang.so"', install['run'])


APT_OWNERS = {
    ('ci.yml', 'host'): ('Install host system dependencies', ['libdbus-1-dev', 'pkg-config']),
    ('ci.yml', 'qemu-rynk'): ('Install QEMU', ['qemu-system-misc', 'libdbus-1-dev', 'pkg-config']),
    ('ci.yml', 'build'): ('Install ARM build dependencies',
                        ['gcc-arm-none-eabi', 'libnewlib-dev', 'clang', 'libclang-dev', 'llvm-dev']),
    ('build.yml', 'validate-profile'): ('Install validation tools', ['ripgrep', 'jq']),
    ('build.yml', 'build'): ('Install ARM toolchain',
                           ['gcc-arm-none-eabi', 'binutils-arm-none-eabi', 'libnewlib-arm-none-eabi', 'clang', 'libclang-dev']),
    ('build.yml', 'build-qube'): ('Install ARM toolchain',
                                ['gcc-arm-none-eabi', 'binutils-arm-none-eabi', 'libnewlib-arm-none-eabi', 'clang', 'libclang-dev']),
    ('build.yml', 'build-settings-reset'): ('Install ARM toolchain',
                                          ['gcc-arm-none-eabi', 'binutils-arm-none-eabi', 'libnewlib-arm-none-eabi']),
}


class AptInstallationTests(unittest.TestCase):
    def install_step(self, workflow, owner):
        job = yaml.safe_load((ROOT / '.github/workflows' / workflow).read_text())['jobs'][owner]
        name, _ = APT_OWNERS[(workflow, owner)]
        steps = [step for step in job['steps'] if step.get('name') == name]
        self.assertEqual(len(steps), 1)
        return steps[0]

    def execute(self, body, mode):
        # Run the real workflow/helper; substitute only privileged APT and the
        # unchanged LLVM/QEMU post-install controls. This is wiring, not a build.
        with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as temporary:
            temp = pathlib.Path(temporary)
            (temp / 'libclang.so').touch()
            recorder = temp / 'apt-fixture.py'
            recorder.write_text(r'''
import json
import os
import sys
args = sys.argv[1:]
with open(os.environ['APT_CALLS'], 'a') as output:
    output.write(json.dumps(args) + '\n')
action = next(arg for arg in args if arg in ('update', 'install'))
values = dict(args[i + 1].split('=', 1) for i, arg in enumerate(args[:-1]) if arg == '-o')
mode = os.environ['APT_FAULT']
if mode == 'stale' and action == 'update':
    print('W: Some index files failed to download.', file=sys.stderr)
    sys.exit(100 if values.get('APT::Update::Error-Mode') == 'any' else 0)
if action == 'install':
    if mode == 'unavailable':
        print('E: Failed to fetch fixture package', file=sys.stderr)
        sys.exit(100)
    if mode == 'transient' and not (
        int(values.get('Acquire::Retries', '1')) >= 3
        and int(values.get('Acquire::http::Timeout', '15')) >= 60
        and int(values.get('Acquire::https::Timeout', '15')) >= 60
    ):
        print('E: fixture transfer exceeded image acquire budget', file=sys.stderr)
        sys.exit(100)
''')
            shell = r'''
sudo() { "$@"; }
apt-get() { /usr/bin/python3 "$APT_FIXTURE" "$@"; }
export -f sudo apt-get
function /usr/bin/llvm-config { printf '%s\n' "$APT_TEMP"; }
qemu-system-riscv32() { printf 'fixture QEMU version\n'; }
eval "$APT_BODY"
'''
            log = temp / 'calls.jsonl'
            result = subprocess.run(['bash', '-euo', 'pipefail', '-c', shell], cwd=ROOT,
                                    env={**os.environ, 'APT_FIXTURE': str(recorder), 'APT_CALLS': str(log),
                                         'APT_FAULT': mode, 'APT_BODY': body, 'APT_TEMP': str(temp),
                                         'GITHUB_ENV': str(temp / 'environment')},
                                    capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def test_transport_budget_reaches_all_seven_installers(self):
        for workflow, owner in APT_OWNERS:
            with self.subTest(workflow=workflow, owner=owner):
                result, calls = self.execute(self.install_step(workflow, owner)['run'], 'transient')
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(len(calls), 2)

    def test_failed_update_cannot_install_from_stale_indexes(self):
        for workflow, owner in APT_OWNERS:
            with self.subTest(workflow=workflow, owner=owner):
                result, calls = self.execute(self.install_step(workflow, owner)['run'], 'stale')
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(calls), 1, 'Install must not run after an incomplete index refresh')

    def test_permanent_failure_still_blocks_each_owner(self):
        for workflow, owner in APT_OWNERS:
            with self.subTest(workflow=workflow, owner=owner):
                result, calls = self.execute(self.install_step(workflow, owner)['run'], 'unavailable')
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(calls), 2)

    def test_healthy_path_preserves_packages_and_trust(self):
        for (workflow, owner), (_, packages) in APT_OWNERS.items():
            with self.subTest(workflow=workflow, owner=owner):
                result, calls = self.execute(self.install_step(workflow, owner)['run'], 'healthy')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(calls), 2)
                install = calls[1]
                self.assertEqual(install[install.index('--') + 1:], packages)
                self.assertIn('--no-install-recommends', install)
                self.assertIn('-y', install)
                self.assertFalse(any('AllowUnauthenticated' in arg or 'Verify-Peer=false' in arg
                                     or '--allow-unauthenticated' in arg for call in calls for arg in call))

    def test_every_owner_uses_one_bounded_shared_installer(self):
        for workflow, owner in APT_OWNERS:
            with self.subTest(workflow=workflow, owner=owner):
                step = self.install_step(workflow, owner)
                self.assertEqual(step.get('timeout-minutes'), 10)
                self.assertIn('bash .github/ci/install-system-deps.sh ', step['run'])
                self.assertNotIn('continue-on-error', step)

    def test_missing_package_arguments_fail_without_apt(self):
        result, calls = self.execute('bash .github/ci/install-system-deps.sh', 'healthy')
        self.assertEqual(result.returncode, 2)
        self.assertEqual(calls, [])


if __name__ == '__main__':
    unittest.main()
