"""Stable CI selection contracts; no compiler, installation or network access."""
import copy
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import tomllib
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
# Complete pre-stabilization argv inventories, with only the stable selector
# normalized. Review every feature/filter/flag change before refreshing these.
ROUTING_DIGESTS = {
    'check.sh': '4410a24d04946ede4688a5fa546e67bde21333d30c2358e9cdbfb4fd74ac2e32',
    'test.sh': '1e8d12284d534442c88c8f23a854cf78406e3a9a87c2bb764316c589fac52e93',
    'host.sh': '69c395a139754b8c23891a2cea9a67193dbbce81deb247a0b80f57550d3c3478',
}


def channel(root=None):
    return tomllib.loads(((root or ROOT) / 'rust-toolchain.toml').read_text())['toolchain']['channel']


def route_commands(script, *args):
    # Execute the real shell owner with inert command recorders. These fixtures
    # test argv routing, not compilation, native tests, wasm or TypeScript.
    with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as temp:
        root = pathlib.Path(temp)
        (root / '.github/ci').mkdir(parents=True)
        (root / 'rynk/rynk-wasm').mkdir(parents=True)
        (root / 'rust-toolchain.toml').write_bytes((ROOT / 'rust-toolchain.toml').read_bytes())
        for name in ('_lib.sh', script):
            (root / '.github/ci' / name).write_bytes((ROOT / '.github/ci' / name).read_bytes())
        log = root / 'commands'
        fixture = r'''
record() { printf '%s\t' "$@" >> "$COMMAND_LOG"; printf '\n' >> "$COMMAND_LOG"; }
cargo() { record cargo "$@"; }
rustc() {
    record rustc "$@"
    while (( $# )); do
        if [[ "$1" == -o ]]; then
            shift
            printf '#!/bin/bash\nexit 0\n' > "$1"
            chmod +x "$1"
            break
        fi
        shift
    done
}
wasm-pack() { record wasm-pack "$@"; }
npx() { record npx "$@"; }
# Python probes are tested separately; routing must stay compiler-free.
python3() { test -f "$SOURCE_ROOT/$1"; }
export -f record cargo rustc wasm-pack npx python3
bash "$@"
'''
        result = subprocess.run(
            ['bash', '-euc', fixture, 'fixture', str(root / '.github/ci' / script), *args],
            env={**os.environ, 'COMMAND_LOG': str(log), 'SOURCE_ROOT': str(ROOT)}, cwd=root,
            text=True, capture_output=True, timeout=10,
        )
        if result.returncode:
            raise AssertionError(result.stdout + result.stderr)
        return [[arg.replace(str(root), '$ROOT') for arg in line.rstrip('\t').split('\t')]
                for line in log.read_text().splitlines()]


class StableToolchainTests(unittest.TestCase):
    def assert_canonical_installer(self, job):
        steps = job['steps']
        installers = [step for step in steps if step.get('uses') == 'dtolnay/rust-toolchain@stable']
        self.assertEqual(len(installers), 1)
        installer = installers[0]
        self.assertEqual(installer.get('with', {}).get('toolchain'),
                         '${{ steps.toolchain.outputs.channel }}')
        producers = [step for step in steps if step.get('id') == 'toolchain']
        self.assertEqual(len(producers), 1)
        producer = producers[0]
        self.assertNotIn('if', producer)
        self.assertNotIn('continue-on-error', producer)
        self.assertLess(steps.index(steps[0]), steps.index(producer))
        self.assertLess(steps.index(producer), steps.index(installer))
        self.assertEqual(producer['run'], 'source .github/ci/_lib.sh\n'
                         'printf \'channel=%s\\n\' "$RMK_STABLE_TOOLCHAIN" >> "$GITHUB_OUTPUT"\n')
        with tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as temp:
            output = pathlib.Path(temp) / 'output'
            result = subprocess.run(['bash', '-euo', 'pipefail', '-c', producer['run']], cwd=ROOT,
                                    env={**os.environ, 'GITHUB_OUTPUT': str(output)},
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_text(), f'channel={channel()}\n')

    def test_four_installers_consume_the_canonical_bootstrap(self):
        jobs = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']
        for owner in ('check', 'test', 'host', 'qemu-rynk'):
            with self.subTest(owner=owner):
                self.assert_canonical_installer(jobs[owner])

    def test_cargo_expand_is_locked_to_the_characterized_version(self):
        jobs = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']
        steps = [step for step in jobs['test']['steps'] if step.get('name') == 'Install cargo-expand']
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]['run'], 'cargo install --locked cargo-expand --version 1.0.127')

    def test_installers_reject_floating_mismatched_or_absent_selection(self):
        jobs = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']
        for owner in ('check', 'test', 'host', 'qemu-rynk'):
            for value in ('stable', '1.100.0', None):
                with self.subTest(owner=owner, toolchain=value):
                    job = copy.deepcopy(jobs[owner])
                    installer = next(step for step in job['steps']
                                     if step.get('uses') == 'dtolnay/rust-toolchain@stable')
                    installer['with']['toolchain'] = value
                    with self.assertRaises(AssertionError):
                        self.assert_canonical_installer(job)
            job = copy.deepcopy(jobs[owner])
            job['steps'] = [step for step in job['steps'] if step.get('id') != 'toolchain']
            with self.subTest(owner=owner, missing_producer=True), self.assertRaises(AssertionError):
                self.assert_canonical_installer(job)

    def test_qemu_bare_commands_inherit_root_toolchain(self):
        # Static rustup ancestry contract, not an emulator or compiler execution.
        for relative in ('examples/use_rust/qemu-riscv-rynk', 'rynk'):
            current = ROOT / relative
            while current != ROOT:
                self.assertFalse((current / 'rust-toolchain.toml').exists())
                self.assertFalse((current / 'rust-toolchain').exists())
                current = current.parent
        run = (ROOT / 'examples/use_rust/qemu-riscv-rynk/run.py').read_text()
        self.assertIn('["cargo", "build"], cwd=HERE', run)
        self.assertIn('cwd=ROOT,', run)
        self.assertNotIn('+stable', run)
        self.assertNotIn('RUSTUP_TOOLCHAIN', run)
        self.assertRegex(channel(), r'^\d+\.\d+\.\d+$')

    def test_shell_owners_route_every_stable_command(self):
        for script, count in [('check.sh', 37), ('test.sh', 31), ('host.sh', 7)]:
            with self.subTest(script=script):
                commands = route_commands(script, *(['stable'] if script == 'check.sh' else []))
                selected = [row for row in commands if row[0] in ('cargo', 'rustc')]
                self.assertEqual(len(selected), count)
                self.assertTrue(all(row[1] == '+' + channel() for row in selected), selected)
                for row in selected:
                    row[1] = '+stable'
                digest = hashlib.sha256(json.dumps(commands, sort_keys=True).encode()).hexdigest()
                self.assertEqual(digest, ROUTING_DIGESTS[script], script)

    def test_nightly_checks_keep_their_separate_toolchain(self):
        commands = route_commands('check.sh', 'nightly')
        self.assertEqual(len(commands), 2)
        self.assertTrue(all(row[:3] == ['cargo', '+nightly', 'check'] for row in commands))

    def test_bootstrap_rejects_floating_or_missing_channels(self):
        for value in ('stable', 'nightly', '1.99', ''):
            with self.subTest(channel=value), tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR')) as temp:
                root = pathlib.Path(temp)
                (root / '.github/ci').mkdir(parents=True)
                (root / '.github/ci/_lib.sh').write_bytes((ROOT / '.github/ci/_lib.sh').read_bytes())
                (root / 'rust-toolchain.toml').write_text(f'[toolchain]\nchannel = "{value}"\n')
                result = subprocess.run(['bash', '-euc', 'source .github/ci/_lib.sh'],
                                        cwd=root, text=True, capture_output=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('exact stable release', result.stderr)

    def test_root_toolchain_installs_targets_for_inheriting_firmware_profiles(self):
        jobs = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())['jobs']
        root_targets = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['targets']
        inherited = []
        for owner in ('build', 'build-qube'):
            for row in jobs[owner]['strategy']['matrix']['include']:
                directory = ROOT / 'keyboards' / row['keyboard']
                current = directory
                while current != ROOT and not (current / 'rust-toolchain.toml').exists():
                    self.assertFalse((current / 'rust-toolchain').exists())
                    current = current.parent
                if current != ROOT:
                    continue
                inherited.append(row['output_name'])
                target = tomllib.loads((directory / '.cargo/config.toml').read_text())['build']['target']
                with self.subTest(profile=row['output_name'], target=target):
                    self.assertEqual(target, row.get('target', 'thumbv7em-none-eabihf'))
                    self.assertIn(target, root_targets,
                                  'The exact root compiler must install its own firmware target')
        self.assertTrue(inherited)

    def test_bootstrap_exports_exact_canonical_release(self):
        expected = channel()
        self.assertRegex(expected, r'^\d+\.\d+\.\d+$')
        result = subprocess.run(
            ['bash', '-euc', 'source .github/ci/_lib.sh; '
             'bash -c \'printf "%s" "$RMK_STABLE_TOOLCHAIN"\''],
            cwd=ROOT, env={**os.environ, 'RMK_STABLE_TOOLCHAIN': 'wrong'},
            text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, expected)


if __name__ == '__main__':
    unittest.main()
