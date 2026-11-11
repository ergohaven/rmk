#!/usr/bin/env python3
"""Check the keycode and subscriber contracts for pointing settings keys."""
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
SHARED = ["EH_STK_TG", "EH_STK_ON", "EH_STK_OFF"]
SIDES = [f"EH_{side}_STK_{action}" for side in ("L", "R") for action in ("TG", "ON", "OFF")]

for board, suffixes, first, sides in [
    ("k04", ("", "_mini", "_micro", "_universal", "_micro_universal", "_qube", "_qube_mini", "_qube_micro"), 41, True),
    ("phenom", ("", "_mini", "_micro"), 41, True),
    ("velvet", ("", "_universal"), 13, False),
    ("classic_qube", ("_velvet",), 13, False),
]:
    for suffix in suffixes:
        path = ROOT / f"keyboards/{board}/vial{suffix}.json"
        codes = json.loads(path.read_text())["customKeycodes"]
        assert [entry["name"] for entry in codes[first:]] == SHARED + (SIDES if sides else []), path

for board, suffixes, minimum in [
    ("k04", ("", "_mini", "_micro", "_vial", "_micro_vial", "_qube", "_qube_mini", "_qube_micro"), 3),
    ("phenom", ("", "_mini", "_micro"), 2),
    ("velvet", ("", "_vial"), 3),
    ("classic_qube", ("_velvet",), 3),
]:
    for suffix in suffixes:
        path = ROOT / f"keyboards/{board}/keyboard{suffix}.toml"
        capacity = re.search(r"^action\.subs\s*=\s*(\d+)", path.read_text(), re.M)
        assert capacity and int(capacity.group(1)) >= minimum, path

print("PASS sticky keycode slots and action subscriber capacity for all pointing profiles")
