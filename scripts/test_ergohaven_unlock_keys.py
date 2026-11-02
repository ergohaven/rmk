#!/usr/bin/env python3
"""Validate Vial unlock on the fixed factory D/F locations of every release profile."""
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


def release_profiles():
    """Read only the production matrix entries, excluding destructive utilities."""
    section = None
    current = None
    profiles = []
    for line in (ROOT / ".github/workflows/build.yml").read_text().splitlines():
        if line == "  build:":
            section = "standalone"
        elif line == "  build-qube:":
            section = "qube"
        elif line.startswith("  ") and not line.startswith("   ") and line.endswith(":"):
            section = None
        if section and line.startswith("          - keyboard: "):
            current = {"keyboard": line.split(": ", 1)[1].strip(), "mode": section}
            profiles.append(current)
        elif current and line.startswith("            ") and not line.startswith("              ") and ":" in line:
            key, value = line.strip().split(":", 1)
            current[key] = value.strip().strip('"')
        elif line == "    steps:":
            current = None
    return profiles


def factory_df_positions(config):
    coordinates = [tuple(map(int, pair)) for pair in re.findall(r"\((\d+),(\d+),[^)]+\)", config["layout"]["map"])]
    keys = config["keymap"]["layer"][0]["keys"].split()
    assert len(keys) == len(coordinates), "factory key tokens must match physical layout map"
    assert len(set(coordinates)) == len(coordinates), "duplicate matrix position"
    result = []
    for letter in ("D", "F"):
        # Micro/OP36 factory home-row mods still have D/F as their tap action.
        matches = [coordinates[index] for index, key in enumerate(keys)
                   if key == letter or re.fullmatch(r"MT\(" + letter + r",[^()]+\)", key)]
        assert len(matches) == 1, f"expected exactly one factory {letter}, got {matches}"
        result.append(list(matches[0]))
    return result


class FactoryUnlockTests(unittest.TestCase):
    def test_all_production_profiles_unlock_on_factory_d_and_f(self):
        profiles = release_profiles()
        self.assertEqual(len(profiles), 17)
        self.assertEqual(len({profile["output_name"] for profile in profiles}), 17)
        for profile in profiles:
            path = ROOT / "keyboards" / profile["keyboard"] / profile["keyboard_toml"]
            with self.subTest(profile=profile["output_name"], path=str(path.relative_to(ROOT))):
                config = tomllib.loads(path.read_text())
                expected = factory_df_positions(config)
                host = config["host"]
                # Exactly one configurator protocol; Vial is on unless turned off.
                self.assertNotEqual(host.get("vial_enabled", True), host.get("rynk_enabled", False))
                self.assertFalse(host.get("insecure", host.get("vial_insecure", False)))
                self.assertEqual(config["host"]["unlock_keys"], expected)
                self.assertNotEqual(expected[0], expected[1])
                for row, col in expected:
                    self.assertLess(row, config["layout"]["rows"])
                    self.assertLess(col, config["layout"]["cols"])

    def test_factory_home_row_mod_taps_are_recognized_without_changing_layers(self):
        fixture = {
            "layout": {"map": "(1,2,L) (1,1,L)"},
            "keymap": {"layer": [{"keys": "MT(D,LShift) MT(F,LCtrl)"}]},
        }
        self.assertEqual(factory_df_positions(fixture), [[1, 2], [1, 1]])
        self.assertEqual(fixture["keymap"]["layer"][0]["keys"], "MT(D,LShift) MT(F,LCtrl)")


if __name__ == "__main__":
    unittest.main(verbosity=2)
