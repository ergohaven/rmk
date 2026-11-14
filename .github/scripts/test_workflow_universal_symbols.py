"""Standalone Universal Symbols feature wiring; requires only PyYAML."""
import hashlib
import pathlib
import shlex
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]


class WorkflowUniversalSymbolsTests(unittest.TestCase):
    def test_standalone_features_match_approved_profiles(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())
        entries = workflow['jobs']['build']['strategy']['matrix']['include']
        expected = {
            'k03': [],
            'k04_series_k04': ['production_v30g', 'universal_mac_iso'],
            'k04_series_micro': ['production_v30g', 'universal_mac_iso'],
            'imperial44': [],
            'op36': [],
            'velvet': [],
            # Mini is not approved for the ANSI/ISO selector.
            'k04_series_mini': ['production_v30g'],
        }
        for output_name, features in expected.items():
            with self.subTest(output_name=output_name):
                matching = [entry for entry in entries
                            if entry['output_name'] == output_name]
                self.assertEqual(len(matching), 1)
                arguments = shlex.split(matching[0].get('build_features', ''))
                self.assertEqual(arguments, ['--features', ','.join(features)] if features else [])
        mini = next(entry for entry in entries
                    if entry['output_name'] == 'k04_series_mini')
        self.assertEqual(mini, {
            'keyboard': 'k04',
            'output_name': 'k04_series_mini',
            'keyboard_toml': 'keyboard_mini.toml',
            'vial_json': 'vial_mini.json',
            'build_features': '--features production_v30g',
        })


    def test_qube_descriptors_preserve_pre_iso_contracts(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())
        entries = workflow['jobs']['build-qube']['strategy']['matrix']['include']
        # Reviewed definitions from 155526aa; Velvet appends Sticky Mode keys.
        # Release 0.3.0 changes only the two version fields; pin all other bytes.
        expected = {
            'k03': '98f9f7b713c90b660ed6a264d15f8220335194f57b1170f6bb59e0610714078e',
            'velvet': '0f213765b7be5d45706eb9c05da4aa4956e9c6ed3f902741643588ff403387d3',
            'imperial44': '34af16939694734c98ec758a9fcc2fb310b76549178df8cd7df658246555ff86',
        }
        for model, digest in expected.items():
            with self.subTest(model=model):
                entry = next(e for e in entries if e['output_name'] == model + '_qube')
                self.assertEqual(entry['vial_json'], 'vial_' + model + '.json')
                definition = ROOT / 'keyboards' / entry['keyboard'] / entry['vial_json']
                self.assertEqual(hashlib.sha256(definition.read_bytes()).hexdigest(), digest)


if __name__ == '__main__':
    unittest.main()
