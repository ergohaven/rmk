#!/usr/bin/env python3
"""Run the actual K:04 settings codec and migration test on the host.

The firmware binary has test=false and targets Cortex-M. Extract only its
unchanged codec definitions into a temporary rustc test harness: never
reimplement the migration or the serializer/deserializer in the test runner.
"""
from pathlib import Path
import json
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / "keyboards/k04/src/layer_names.rs").read_text()


def settings_with_qsid(value, qsid):
    if isinstance(value, dict):
        if value.get("qsid") == qsid:
            yield value
        for child in value.values():
            yield from settings_with_qsid(child, qsid)
    elif isinstance(value, list):
        for child in value:
            yield from settings_with_qsid(child, qsid)


for profile in ("", "_mini", "_micro"):
    for suffix, values in (
        ("", [50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 750, 1000, 1250, 1500]),
        ("_qube", [250, 500, 750, 1000, 1250, 1500]),
    ):
        path = ROOT / f"keyboards/k04/vial{suffix}{profile}.json"
        definition = json.loads(path.read_text())
        settings = list(settings_with_qsid(definition, 324))
        assert len(settings) == 1, path
        assert settings[0]["variants"] == [f"{ms} ms" for ms in values], path


def between(start: str, end: str) -> str:
    assert source.count(start) == 1, start
    assert source.count(end) == 1, end
    return source.split(start, 1)[1].split(end, 1)[0]


def function(name: str) -> str:
    match = re.search(r"^(?:const )?fn " + re.escape(name) + r"\b", source, re.M)
    assert match is not None, name
    opening = source.index("{", match.start())
    depth = 0
    for pos in range(opening, len(source)):
        if source[pos] == "{":
            depth += 1
        elif source[pos] == "}":
            depth -= 1
            if depth == 0:
                return source[match.start(): pos + 1]
    raise AssertionError(f"Unclosed function {name}")


codec_constants = "const MODULE_SETTINGS_VERSION" + between(
    "const MODULE_SETTINGS_VERSION", "const _: () = assert!(MODULE_DEFAULTS[IDX_FLAGS]"
)
functions = [
    "serialize_module_settings", "deserialize_module_settings",
    "ensure_module_settings_initialized", "reset_module_settings", "module_byte",
    "module_set_byte", "module_layer_color_index", "module_set_layer_color_index",
    "module_bt_profile_color_index", "module_set_bt_profile_color_index",
    "pack_color", "unpack_color", "set_default_layer_color",
]
test_marker = '#[cfg(all(test, feature = "standalone"))]\nmod timeout_migration_tests'
assert source.count(test_marker) == 1
tests = source[source.index(test_marker):]
assert tests.rstrip().endswith("}")
harness = "\n".join([
    "#![allow(dead_code)]",
    "use std::sync::atomic::{AtomicU8, Ordering};",
    codec_constants,
    "static MODULE_SETTINGS: [AtomicU8; MODULE_SETTINGS_LEN] = [const { AtomicU8::new(0) }; MODULE_SETTINGS_LEN];",
    *(function(name) for name in functions),
    tests,
])
with tempfile.TemporaryDirectory(prefix="k04-settings-migration-") as directory:
    source_file = Path(directory) / "migration.rs"
    source_file.write_text(harness)
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo/bin/rustc")
    for profile, features in (("standalone", ["--cfg", 'feature="standalone"']), ("qube", [])):
        test_binary = Path(directory) / f"migration-test-{profile}"
        subprocess.run([rustc, "--edition=2021", *features, "--test", str(source_file), "-o", str(test_binary)], check=True)
        subprocess.run([str(test_binary)], check=True)
