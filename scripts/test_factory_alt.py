#!/usr/bin/env python3
"""Factory Alt must send Left Alt, including mod-tap holds on every layer."""
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
EXPECTED = {
    'classic_qube': ('', '_k03', '_imperial44', '_velvet'),
    'imperial44': ('', '_vial'),
    'k03': ('', '_vial'),
    'k04': ('', '_mini', '_micro', '_vial', '_micro_vial', '_qube', '_qube_mini', '_qube_micro'),
    'op36': ('', '_vial'),
    'phenom': ('', '_mini', '_micro'),
    'velvet': ('', '_vial'),
}


class FactoryAltTests(unittest.TestCase):
    def profiles(self):
        expected = {ROOT / 'keyboards' / model / f'keyboard{suffix}.toml'
                    for model, suffixes in EXPECTED.items() for suffix in suffixes}
        self.assertEqual(set((ROOT / 'keyboards').glob('*/keyboard*.toml')), expected)
        return sorted(expected)

    def test_all_factory_layers_use_left_alt_instead_of_right_alt(self):
        for path in self.profiles():
            config = tomllib.loads(path.read_text())
            for index, layer in enumerate(config['keymap']['layer']):
                with self.subTest(profile=str(path.relative_to(ROOT)), layer=index):
                    self.assertNotRegex(layer['keys'], r'\bRAlt\b')

    def test_every_factory_base_has_left_alt(self):
        for path in self.profiles():
            with self.subTest(profile=str(path.relative_to(ROOT))):
                base = tomllib.loads(path.read_text())['keymap']['layer'][0]
                self.assertRegex(base['keys'], r'\bLAlt\b')


if __name__ == '__main__':
    unittest.main(verbosity=2)
