"""Exercise the real profile validator's settings and factory-name guards."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/check_ergohaven_profile.sh"
NAME_START = 'python3 - "$default_names_source" <<\'PY\' || fail "$default_names_source: factory layer-name profiles drifted"\n'


class ProfileValidatorGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.descriptors = ["keyboards/k04/vial.json"] + [
            f"keyboards/phenom/vial{model}{geometry}.json"
            for model in ("", "_mini", "_micro") for geometry in ("", "_universal")
        ]
        for name in self.descriptors + ["keyboards/common/default_layer_names.rs"]:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((ROOT / name).read_bytes())
        source = SCRIPT.read_text()
        self.settings_code = 'k04_module_tabs=' + source.split('k04_module_tabs=', 1)[1].split('\ndone', 1)[0] + '\ndone\n'
        self.names_code = source.split(NAME_START, 1)[1].split('\nPY', 1)[0]

    def run_settings(self):
        code = 'set -euo pipefail\nfail() { echo "$*" >&2; exit 1; }\n' + self.settings_code
        return subprocess.run(['bash', '-c', code], cwd=self.root, text=True, capture_output=True)

    def run_names(self):
        return subprocess.run([sys.executable, '-', 'keyboards/common/default_layer_names.rs'],
                              input=self.names_code, cwd=self.root, text=True, capture_output=True)

    def test_current_complete_settings_pass(self):
        result = self.run_settings()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_settings_drift_is_rejected_for_each_descriptor(self):
        for name in self.descriptors[1:]:
            path = self.root / name
            original = path.read_bytes()
            for mutation in ('auto', 'none', 'qsid', 'led', 'missing_field'):
                with self.subTest(path=name, mutation=mutation):
                    data = json.loads(original)
                    field = data['settings'][0]['fields'][0]
                    if mutation == 'auto':
                        field['variants'].append('Auto')
                    elif mutation == 'none':
                        field['min'] = 0
                    elif mutation == 'qsid':
                        field['qsid'] = 150
                    elif mutation == 'led':
                        data['settings'][-1]['fields'][0]['max'] = 254
                    else:
                        data['settings'][1]['fields'].pop()
                    path.write_text(json.dumps(data))
                    result = self.run_settings()
                    path.write_bytes(original)
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn(name, result.stderr)

    def test_universal_layout_references_must_be_reindexed(self):
        # Current tabs have no layoutOption; seed one to exercise the retained transform.
        for name in self.descriptors:
            path = self.root / name
            data = json.loads(path.read_text())
            data['settings'][0]['fields'][0]['layoutOption'] = int('_universal' in name)
            path.write_text(json.dumps(data))
        result = self.run_settings()
        self.assertEqual(result.returncode, 0, result.stderr)
        path = self.root / 'keyboards/phenom/vial_mini_universal.json'
        data = json.loads(path.read_text())
        data['settings'][0]['fields'][0]['layoutOption'] = 0
        path.write_text(json.dumps(data))
        result = self.run_settings()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(path.name, result.stderr)

    def test_current_const_factory_names_pass(self):
        result = self.run_names()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_factory_names_and_unused_slots_are_guarded(self):
        path = self.root / 'keyboards/common/default_layer_names.rs'
        original = path.read_text()
        for profile, assignment in (
            ('STANDARD_NO_MOUSE', 'names[1] = "Navigation";'),
            ('STANDARD_NO_MOUSE', 'names[4] = "4";'),
            ('STANDARD_WITH_MOUSE', 'names[4] = "Pointing";'),
            ('STANDARD_WITH_MOUSE', 'names[15] = "15";'),
            ('PHENOM', 'names[3] = "Adjust";'),
            ('PHENOM', 'names[4] = "";'),
            ('PHENOM', 'names[5] = "5";'),
            ('PHENOM', 'names.reverse();'),
        ):
            with self.subTest(profile=profile, assignment=assignment):
                match = re.search(rf'pub const {profile}:.*?=\s*\{{(.*?)\}};', original, re.DOTALL)
                assert match is not None, profile
                body = match.group(1).replace('    names\n', f'    {assignment}\n    names\n')
                path.write_text(original[:match.start(1)] + body + original[match.end(1):])
                result = self.run_names()
                path.write_text(original)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn('AssertionError', result.stderr)


if __name__ == '__main__':
    unittest.main()
