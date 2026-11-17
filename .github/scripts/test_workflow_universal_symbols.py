"""All official profiles use the existing packed host layout-option selector."""
import copy
import hashlib
import itertools
import json
import pathlib
import shlex
import subprocess
import tomllib
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
BASELINE = json.loads((ROOT / '.github/scripts/fixtures/universal_geometry_baseline.json').read_text())
PROFILE_BASELINE = json.loads((ROOT / '.github/scripts/fixtures/universal_profile_baseline.json').read_text())
GEOMETRY = ['Universal Symbols geometry', 'ANSI', 'ISO']
ISO_FEATURES = {'universal_symbols_iso', 'universal_symbols_iso_bit0', 'universal_symbols_iso_bit1'}
# Keep the immutable 22-profile fixture for manual Vial and historical proofs.
OFFICIAL_BASELINE_ROWS = {job: [row for row in entries if not row['output_name'].endswith('_vial')]
                          for job, entries in BASELINE['rows'].items()}


def rows():
    jobs = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())['jobs']
    return {job: jobs[job]['strategy']['matrix']['include'] for job in BASELINE['rows']}


def width(label):
    return 1 if isinstance(label, str) else (len(label) - 2).bit_length()


def offsets(labels):
    return [sum(map(width, labels[index + 1:])) for index in range(len(labels))]


def closure(manifest, arguments, root_features):
    """Resolve local Cargo aliases and RMK dependency features, without Cargo."""
    args = shlex.split(arguments)
    pending = [] if '--no-default-features' in args else ['default']
    if '--features' in args:
        pending += args[args.index('--features') + 1].split(',')
    local, rmk = set(), set(manifest['dependencies']['rmk'].get('features', []))
    while pending:
        feature = pending.pop()
        if feature.startswith('rmk/'):
            rmk.add(feature.split('/', 1)[1])
        elif feature not in local:
            local.add(feature)
            pending += manifest['features'].get(feature, [])
    pending = list(rmk)
    while pending:
        feature = pending.pop()
        for child in root_features.get(feature, []):
            if '/' not in child and not child.startswith('dep:') and child not in rmk:
                rmk.add(child)
                pending.append(child)
    return local, rmk


def descriptor(row):
    return json.loads((ROOT / 'keyboards' / row['keyboard'] / row['vial_json']).read_text())


def strings(definition):
    return [key for row in definition['layouts']['keymap'] for key in row if isinstance(key, str)]


def visible(key, values):
    fields = key.split('\n')
    if len(fields) < 4 or not fields[3]:
        return True
    index, value = map(int, fields[3].split(','))
    return values[index] == value


class WorkflowUniversalSymbolsTests(unittest.TestCase):
    def test_complete_inventory_and_unchanged_non_geometry_matrix(self):
        current = rows()
        self.assertEqual(sum(map(len, current.values())), 17)
        for job, old_rows in OFFICIAL_BASELINE_ROWS.items():
            self.assertEqual([row['output_name'] for row in current[job]],
                             [row['output_name'] for row in old_rows])
            for old, new in zip(old_rows, current[job]):
                with self.subTest(profile=new['output_name']):
                    # Only descriptor and feature arguments are authorized to change.
                    self.assertEqual({k: v for k, v in new.items() if k not in
                                      ('vial_json', 'build_features', 'half_features', 'qube_features')},
                                     {k: v for k, v in old.items() if k not in
                                      ('vial_json', 'build_features', 'half_features', 'qube_features')})
                    self.assertTrue(new['vial_json'].endswith('_universal.json'))
                    self.assertEqual(descriptor(new)['layouts']['labels'][0], GEOMETRY)

    def test_every_role_resolves_exact_mask_without_protocol_or_hardware_changes(self):
        root = tomllib.loads((ROOT / 'rmk/Cargo.toml').read_text())['features']
        for job, entries in rows().items():
            for old, row in zip(OFFICIAL_BASELINE_ROWS[job], entries):
                manifest = tomllib.loads((ROOT / 'keyboards' / row['keyboard'] / 'Cargo.toml').read_text())
                previous = BASELINE['manifests'][row['keyboard']]
                labels = descriptor(row).get('layouts', {}).get('labels', [])
                self.assertTrue(labels and labels[0] == GEOMETRY, row['output_name'])
                expected_mask = 1 << sum(map(width, labels[1:]))
                roles = [('central', 'build_features'), ('peripheral', 'build_features')] if job == 'build' else [
                    ('qube', 'qube_features'), ('left', 'half_features'), ('right', 'half_features')]
                for role, field in roles:
                    with self.subTest(profile=row['output_name'], role=role):
                        prefix = '--no-default-features ' if role == 'qube' else row.get('half_no_default', '') + ' ' if job == 'build-qube' else ''
                        old_arguments = '--no-default-features --features qube' if role == 'qube' else prefix + old.get(field, '')
                        local, resolved = closure(manifest, prefix + row.get(field, ''), root)
                        old_local, old_resolved = closure(previous, old_arguments, root)
                        self.assertIn('universal_symbols_iso', resolved)
                        self.assertFalse({'universal_symbols_iso_bit0', 'universal_symbols_iso_bit1'} <= resolved)
                        actual_mask = 1 if 'universal_symbols_iso_bit0' in resolved else 2 if 'universal_symbols_iso_bit1' in resolved else 4
                        self.assertEqual(actual_mask, expected_mask)
                        added = set(PROFILE_BASELINE['additional_rmk_features'].get(row['keyboard'], []))
                        self.assertEqual(resolved - ISO_FEATURES, (old_resolved - ISO_FEATURES) | added)
                        self.assertEqual(local - {'universal_mac_iso'}, old_local - {'universal_mac_iso'})

    def test_reviewed_profile_updates_have_exact_scope_and_keep_historical_hashes(self):
        paths = {f"keyboards/{row['keyboard']}/{row['vial_json']}"
                 for entries in OFFICIAL_BASELINE_ROWS.values() for row in entries}
        boards = {pathlib.PurePosixPath(path).parts[1] for path in paths}
        self.assertEqual(PROFILE_BASELINE['previous_descriptor_hashes'],
                         {path: BASELINE['descriptors'][path] for path in paths})
        self.assertEqual(set(PROFILE_BASELINE['descriptor_hashes']), paths)
        expected_features = {board: ['ergohaven_language_helpers'] for board in boards}
        expected_features['phenom'].insert(0, 'ergohaven_window_switching')
        self.assertEqual(PROFILE_BASELINE['additional_rmk_features'], expected_features)

    def test_phenom_production_profiles_advertise_auto_after_manual_module_values(self):
        expected = ['None', 'Encoder', 'Trackball', 'Touchpad', 'Auto']
        profiles = []
        for entries in rows().values():
            for row in entries:
                if row['keyboard'] != 'phenom':
                    continue
                profiles.append(row['output_name'])
                with self.subTest(profile=row['output_name']):
                    choices = [(field['qsid'], field['variants'])
                               for group in descriptor(row)['settings'] for field in group['fields']
                               if field.get('qsid') in (149, 150)]
                    self.assertEqual(choices, [(149, expected), (150, expected)])
        self.assertCountEqual(profiles, ['phenom', 'phenom_mini', 'phenom_micro'])

    def test_descriptors_only_prepend_selector_and_reindex_conditions(self):
        for job, entries in rows().items():
            for old, row in zip(OFFICIAL_BASELINE_ROWS[job], entries):
                with self.subTest(profile=row['output_name']):
                    path = str(pathlib.Path('keyboards') / old['keyboard'] / old['vial_json'])
                    original_bytes = (ROOT / path).read_bytes()
                    # Reviewed product metadata evolves; the immutable geometry
                    # reference remains available and all current bytes stay pinned.
                    digest = PROFILE_BASELINE['descriptor_hashes'].get(path, BASELINE['descriptors'][path])
                    self.assertEqual(hashlib.sha256(original_bytes).hexdigest(), digest)
                    original = json.loads(original_bytes)
                    current = descriptor(row)
                    self.assertEqual(current['layouts'].get('labels', [None])[0], GEOMETRY)
                    if original['layouts'].get('labels', [None])[0] == GEOMETRY:
                        self.assertEqual(current, original)
                        continue
                    restored = copy.deepcopy(current)
                    restored['layouts']['labels'].pop(0)
                    if 'labels' not in original['layouts']:
                        del restored['layouts']['labels']
                    # Device-setting references are option INDICES, not stored bits.
                    for group in restored.get('settings', []):
                        for field in group.get('fields', []):
                            if 'layoutOption' in field:
                                field['layoutOption'] -= 1
                    for keyrow in restored['layouts']['keymap']:
                        for index, key in enumerate(keyrow):
                            if not isinstance(key, str):
                                continue
                            fields = key.split('\n')
                            if len(fields) > 3 and fields[3]:
                                option, value = fields[3].split(',')
                                self.assertGreater(int(option), 0)
                                fields[3] = f'{int(option) - 1},{value}'
                                keyrow[index] = '\n'.join(fields)
                    # Includes exact customKeycodes, USB/name/version/entropy metadata,
                    # matrix coordinates, encoder markers and every unrelated field.
                    self.assertEqual(restored, original)

    def test_all_old_packed_options_and_visibility_are_identical_for_both_geometries(self):
        for job, entries in rows().items():
            for old, row in zip(OFFICIAL_BASELINE_ROWS[job], entries):
                original, current = descriptor(old), descriptor(row)
                labels = original['layouts'].get('labels', [])
                if labels and labels[0] == GEOMETRY:
                    continue
                with self.subTest(profile=row['output_name']):
                    new_labels = current['layouts'].get('labels', [])
                    self.assertEqual(new_labels, [GEOMETRY] + labels)
                    self.assertEqual(offsets(labels), offsets(new_labels)[1:])
                    old_keys, new_keys = strings(original), strings(current)
                    self.assertEqual(len(old_keys), len(new_keys))
                    for values in itertools.product(*[range(2 if isinstance(label, str) else len(label) - 1) for label in labels]):
                        old_bits = sum(value << offset for value, offset in zip(values, offsets(labels)))
                        mask = 1 << offsets(new_labels)[0]
                        self.assertEqual(old_bits & mask, 0)  # Existing settings default to ANSI.
                        for geometry in (0, 1):
                            packed = old_bits | geometry * mask
                            self.assertEqual(packed & (mask - 1), old_bits)
                            self.assertEqual(bool(packed & mask), bool(geometry))
                            self.assertEqual([visible(key, values) for key in old_keys],
                                             [visible(key, (geometry, *values)) for key in new_keys])

    def test_device_setting_option_references_keep_the_same_packed_bit(self):
        for job, entries in rows().items():
            for old, row in zip(OFFICIAL_BASELINE_ROWS[job], entries):
                original, current = descriptor(old), descriptor(row)
                old_labels = original['layouts'].get('labels', [])
                new_labels = current['layouts'].get('labels', [])
                for group, new_group in zip(original.get('settings', []), current.get('settings', [])):
                    for field, new_field in zip(group['fields'], new_group['fields']):
                        if 'layoutOption' in field:
                            with self.subTest(profile=row['output_name']):
                                self.assertEqual(offsets(old_labels)[field['layoutOption']],
                                                 offsets(new_labels)[new_field['layoutOption']])

    def test_runtime_and_storage_schema_match_reviewed_baseline(self):
        # Runtime features can evolve after the geometry change. Keep the
        # historical fixture intact and require a reviewed, complete successor.
        runtime = json.loads((ROOT / '.github/scripts/fixtures/universal_runtime_baseline.json').read_text())
        self.assertEqual(runtime['previous_runtime_hashes'], BASELINE['runtime_hashes'])
        self.assertEqual(set(runtime['runtime_hashes']), set(BASELINE['runtime_hashes']))
        for path, digest in runtime['runtime_hashes'].items():
            self.assertEqual(hashlib.sha256((ROOT / path).read_bytes()).hexdigest(), digest, path)

    def test_protocol_guards_remain_but_do_not_reject_rynk_geometry(self):
        for model in ('k03', 'op36', 'imperial44', 'velvet', 'k04'):
            source = (ROOT / 'keyboards' / model / 'build.rs').read_text()
            with self.subTest(model=model):
                self.assertIn('rynk ^ vial', source)
                self.assertIn('!vial || std::env::var_os("CARGO_FEATURE_VIAL_STANDALONE").is_some()', source)
                self.assertNotIn('universal_mac_iso is only supported by the Vial standalone profile', source)
                self.assertNotIn('Mini/Qube are excluded', source)

    def test_qube_commands_consume_profile_features_on_all_roles(self):
        jobs = yaml.safe_load((ROOT / '.github/workflows/build.yml').read_text())['jobs']
        commands = [step.get('run', '') for step in jobs['build-qube']['steps']]
        self.assertIn('cargo build --release --bin qube --no-default-features ${{ matrix.qube_features }}', commands)
        for role in ('left', 'right'):
            self.assertIn(f'cargo build --release --bin {role} ${{{{ matrix.half_no_default }}}} ${{{{ matrix.half_features }}}}', commands)

    def test_local_matrix_records_the_same_descriptors_and_feature_closures(self):
        # Execute only the script's argument construction. The recorder replaces
        # run(), so no Cargo, compiler, tool discovery or build is launched.
        script = (ROOT / 'scripts/build_k04_matrix.sh').read_text()
        body = script[script.index('run() {'):]
        end = body.index('build_split() {')
        recorder = "run() { printf '%s\\037' \"$@\"; printf '\\n'; }\n"
        result = subprocess.run(['bash', '-c', 'set -eu; repo_root="$1"; BINDGEN_EXTRA_CLANG_ARGS=record-only;\n' + recorder + body[end:], 'record', str(ROOT)],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        root = tomllib.loads((ROOT / 'rmk/Cargo.toml').read_text())['features']
        expected = {(row['keyboard'], row['keyboard_toml']): row
                    for entries in rows().values() for row in entries if not row['output_name'].endswith('_vial')}
        seen = set()
        for line in result.stdout.splitlines():
            if '\x1f' not in line:
                continue
            args = line.rstrip('\x1f').split('\x1f')
            model = pathlib.PurePosixPath(args[0]).name
            values = dict(arg.split('=', 1) for arg in args if '=' in arg)
            config = pathlib.Path(values.get('KEYBOARD_TOML_PATH', 'keyboard.toml')).name
            row = expected[model, config]
            definition = pathlib.Path(values.get('VIAL_JSON_PATH', 'vial.json'))
            self.assertEqual(definition, ROOT / 'keyboards' / model / row['vial_json'], row['output_name'])
            manifest = tomllib.loads((ROOT / 'keyboards' / model / 'Cargo.toml').read_text())
            arguments = args[args.index('cargo') + 2:]
            role = 'qube' if 'qube' in arguments else 'left' if 'left' in arguments else 'central'
            field = 'qube_features' if role == 'qube' else 'half_features' if role == 'left' else 'build_features'
            prefix = '--no-default-features ' if role == 'qube' else row.get('half_no_default', '') + ' ' if role == 'left' else ''
            self.assertEqual(closure(manifest, shlex.join(arguments), root),
                             closure(manifest, prefix + row.get(field, ''), root), row['output_name'])
            seen.add((row['output_name'], role))
        self.assertEqual(len(seen), 24)  # Ten standalone + two commands per seven Qube profiles.


if __name__ == '__main__':
    unittest.main()
