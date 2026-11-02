"""Exercise the production live-matrix guard, not a duplicate predicate."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / 'scripts/check_ergohaven_profile.sh').read_text()
a = SOURCE.index('live_matrix_cargo_manifests=(')
b = SOURCE.index('\n)', a) + 2
c = SOURCE.index('for file in "${live_matrix_cargo_manifests[@]}"; do')
d = SOURCE.index('\ndone', c) + 5
GUARD = ('set -euo pipefail\nfailures=0\n'
         'fail() { printf "profile contract: %s\\n" "$*"; failures=$((failures+1)); }\n'
         + SOURCE[a:b] + '\n' + SOURCE[c:d]
         + '\nprintf "failures=%s\\n" "$failures"\n((failures == 0))\n')
MODELS = ('imperial44', 'k03', 'op36', 'velvet', 'classic_qube')


def manifest(features, dependency_features='"storage"'):
    return ('[package]\nname = "guard_fixture"\nversion = "0.1.0"\n'
            '[dependencies]\nrmk = { path = "../../rmk", default-features = false, '
            'features = [' + dependency_features + '] }\n[features]\n' + features)


SELECTABLE = '''default = ["protocol_rynk"]
protocol_rynk = ["rmk/ergohaven_rynk"]
protocol_vial = ["rmk/vial", "rmk/host_lock"]
vial_standalone = ["protocol_vial"]
'''


class LiveMatrixGuard(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get('TMPDIR'))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for path in ['rmk/Cargo.toml'] + [f'keyboards/{m}/Cargo.toml' for m in MODELS]:
            dest = self.root / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, dest)

    def check(self, expected, fixture=None, failures=None):
        if fixture is not None:
            (self.root / 'keyboards/imperial44/Cargo.toml').write_text(fixture)
        result = subprocess.run(['bash', '-c', GUARD], cwd=self.root,
                                text=True, capture_output=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        self.assertIn(f'failures={expected if failures is None else failures}', result.stdout)
        if expected:
            self.assertIn('host_lock', result.stdout + result.stderr)

    def test_actual_five_manifests(self):
        self.check(0)

    def test_unconditional_rynk_manifests(self):
        for model in MODELS:
            path = self.root / f'keyboards/{model}/Cargo.toml'
            path.write_text(manifest('default = []\n', '"ergohaven_rynk"'))
        self.check(0)

    def test_selectable_protocols(self):
        self.check(0, manifest(SELECTABLE))

    def test_nested_board_aliases_with_cycle(self):
        fixture = SELECTABLE.replace('["rmk/vial", "rmk/host_lock"]', '["locked_vial"]')
        fixture += 'locked_vial = ["cycle", "rmk/vial", "rmk/host_lock"]\ncycle = ["locked_vial"]\n'
        self.check(0, manifest(fixture))

    def test_transitive_rmk_host_lock(self):
        path = self.root / 'rmk/Cargo.toml'
        path.write_text(path.read_text().replace('rynk = ["host", "host_lock",',
                                               'guard_lock = ["host_lock"]\nrynk = ["host", "guard_lock",'))
        self.check(0, manifest(SELECTABLE))

    def test_unconditional_vial_with_host_lock(self):
        self.check(0, manifest('default = []\n', '"vial", "host_lock"'))

    def test_missing_vial_host_lock(self):
        self.check(1, manifest(SELECTABLE.replace(', "rmk/host_lock"', '')))

    def test_unused_host_lock_alias_does_not_count(self):
        self.check(1, manifest(SELECTABLE.replace(', "rmk/host_lock"', '')
                               + 'unused = ["rmk/host_lock"]\n'))

    def test_comment_host_lock_does_not_count(self):
        self.check(1, manifest(SELECTABLE.replace(', "rmk/host_lock"', '')
                               + '# "host_lock" "ergohaven_rynk"\n'))

    def test_other_dependency_host_lock_does_not_count(self):
        self.check(1, manifest(SELECTABLE.replace('rmk/host_lock', 'unrelated/host_lock')))

    def test_similar_feature_name_does_not_count(self):
        fixture = SELECTABLE.replace('rmk/host_lock', 'rmk/not_host_lock')
        self.check(1, manifest(fixture))

    def test_missing_default_protocol(self):
        self.check(1, manifest(SELECTABLE.replace('default = ["protocol_rynk"]', 'default = []')))

    def test_standalone_does_not_select_protocol(self):
        self.check(1, manifest(SELECTABLE.replace('vial_standalone = ["protocol_vial"]',
                                               'vial_standalone = ["rmk/host_lock"]')))

    def test_vial_must_not_also_enable_rynk(self):
        self.check(1, manifest(SELECTABLE.replace('"rmk/vial",', '"rmk/vial", "rmk/rynk",')))

    def test_rynk_must_really_bring_host_lock(self):
        path = self.root / 'rmk/Cargo.toml'
        path.write_text(path.read_text().replace('rynk = ["host", "host_lock",', 'rynk = ["host",'))
        self.check(1, manifest(SELECTABLE), failures=5)

    def test_unconditional_vial_without_host_lock(self):
        self.check(1, manifest('default = []\n', '"vial"'))


if __name__ == '__main__':
    unittest.main()
