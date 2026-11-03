"""Selectable Vial profiles must leave the existing Rynk builds intact."""
import pathlib
import shlex
import tomllib
import unittest
import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
MODELS = ('k04', 'k03', 'imperial44', 'op36', 'velvet')


class VialProfiles(unittest.TestCase):
    def test_protocol_selection_is_explicit_and_keeps_hardware_features(self):
        for model in MODELS:
            with self.subTest(model=model):
                manifest = tomllib.loads((ROOT / 'keyboards' / model / 'Cargo.toml').read_text())
                features = manifest['features']
                self.assertIn('protocol_rynk', features['default'])
                self.assertEqual(features['protocol_rynk'], ['rmk/ergohaven_rynk'])
                self.assertEqual(features['protocol_vial'], ['rmk/vial', 'rmk/host_lock'])
                self.assertNotIn('ergohaven_rynk', manifest['dependencies']['rmk']['features'])
                self.assertIn('nrf52840_ble', manifest['dependencies']['rmk']['features'])
                self.assertIn('split', manifest['dependencies']['rmk']['features'])
                if model == 'k04':
                    self.assertIn('standalone', features['vial_standalone'])
                self.assertIn('protocol_vial', features['vial_standalone'])
                expected_iso = {'k04': 'universal_symbols_iso', 'k03': 'universal_symbols_iso_bit0',
                                'imperial44': 'universal_symbols_iso_bit0', 'op36': 'universal_symbols_iso_bit0',
                                'velvet': 'universal_symbols_iso_bit1'}
                self.assertEqual(features['universal_mac_iso'], ['rmk/' + expected_iso[model]])

    def test_vial_toml_changes_only_protocol(self):
        for model in MODELS:
            names = ['keyboard.toml', 'keyboard_micro.toml'] if model == 'k04' else ['keyboard.toml']
            for name in names:
                with self.subTest(model=model, config=name):
                    original = tomllib.loads((ROOT / 'keyboards' / model / name).read_text())
                    vial = tomllib.loads((ROOT / 'keyboards' / model / name.replace('.toml', '_vial.toml')).read_text())
                    original['host']['vial_enabled'] = True
                    original['host']['rynk_enabled'] = False
                    self.assertEqual(original, vial)

    def test_descriptor_masks_and_build_selectors(self):
        import json
        job = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())['jobs']['build']
        masks = {'k04': 4, 'k03': 1, 'imperial44': 1, 'op36': 1, 'velvet': 2}
        for row in job['strategy']['matrix']['include']:
            if row['output_name'].endswith('_vial'):
                self.assertTrue(row['keyboard_toml'].endswith('_vial.toml'))
                self.assertTrue(row['vial_json'].endswith('_universal.json'))
                descriptor = json.loads((ROOT / 'keyboards' / row['keyboard'] / row['vial_json']).read_text())
                labels = descriptor['layouts']['labels']
                self.assertEqual(labels[0], ['Universal Symbols geometry', 'ANSI', 'ISO'])
                widths = [1 if isinstance(label, str) else (len(label) - 2).bit_length() for label in labels[1:]]
                self.assertEqual(1 << sum(widths), masks[row['keyboard']])

    def test_ci_vial_rows_actually_select_vial(self):
        job = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())['jobs']['build']
        entries = job['strategy']['matrix']['include']
        approved = {'k03', 'k04_series_k04', 'k04_series_micro', 'imperial44', 'op36', 'velvet'}
        for name in approved:
            vial = next(row for row in entries if row['output_name'] == name + '_vial')
            args = shlex.split(vial['build_features'])
            self.assertIn('--no-default-features', args)
            flags = args[args.index('--features') + 1].split(',')
            self.assertIn('vial_standalone', flags)
            self.assertIn('universal_mac_iso', flags)
        for row in entries:
            if not row['output_name'].endswith('_vial'):
                self.assertNotIn('universal_mac_iso', row.get('build_features', ''))
        self.assertFalse(any(row['output_name'] == 'k04_series_mini_vial' for row in entries))


if __name__ == '__main__':
    unittest.main()
