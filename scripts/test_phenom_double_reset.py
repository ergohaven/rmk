#!/usr/bin/env python3
"""Run the Phenom production reset decision with deterministic retained SRAM/time.

Run via agent-storage; only the RP2040 SRAM access, clock and ROM call are
hardware-specific. The decision is included verbatim, not translated to Python.
"""
import argparse
import hashlib
import os
import re
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIRMWARE = ROOT / "keyboards/phenom/src/firmware.rs"
HELPER = ROOT / "keyboards/phenom/src/double_reset.rs"


def check_wiring():
    firmware = FIRMWARE.read_text()
    assert "#[Overwritten(chip_init)]" in firmware, (
        "Phenom physical Reset startup never checks a retained double-reset marker"
    )
    hook_match = re.search(r"#\[Overwritten\(chip_init\)\]\s*fn \w+\(\)\s*\{([^}]+)\}", firmware)
    assert hook_match, "missing synchronous chip-init body"
    hook = hook_match.group(1)
    assert hook.index("::embassy_rp::init(") < hook.index("crate::double_reset::check();")
    assert "let mut p =" in hook
    assert "mod double_reset;" in firmware
    for role in ("central", "peripheral"):
        binary = (FIRMWARE.parent / f"{role}.rs").read_text()
        assert 'include!("firmware.rs");' in binary
        assert "phenom!(" in binary
    # The hook executes before USB, role election or the RMK watchdog starts.
    generated = (ROOT / "rmk-macro/src/codegen/orchestrator.rs").read_text()
    main = generated[generated.index("#main_function_sig {"):]
    assert main.index("#chip_init") < main.index("#usb_init") < main.index("#watchdog_init") < main.index("#run_rmk")
    override = (ROOT / "rmk-macro/src/codegen/chip/chip_init.rs").read_text()
    assert "Overwritten::ChipInit =>" in override
    assert "let stmts = &item_fn.block.stmts;" in override
    helper = HELPER.read_text()
    assert 'if bootloader_requested(&mut rp2040::Window)' in helper
    assert 'embassy_rp::rom_data::reset_to_usb_boot(0, 0);' in helper
    assert '.uninit.phenom_double_reset' in helper and '".space 12"' in helper
    assert 'embassy_time::block_for(' in helper and '.await' not in helper
    assert 'read_volatile(' not in helper and '.assume_init(' not in helper
    for offset in (0, 4, 8):
        assert f'[r0, #{offset}]' in helper


TESTS = r'''
#[derive(Default)]
struct Window {
    marker: [u32; 3],
    elapsed: u64,
    writes: Vec<[u32; 3]>,
    reset_after: Option<u64>,
}
impl ResetWindow for Window {
    fn read_marker(&mut self) -> [u32; 3] { self.marker }
    fn write_marker(&mut self, marker: [u32; 3]) {
        self.marker = marker;
        self.writes.push(marker);
    }
    fn wait_ms(&mut self, millis: u64) {
        assert_eq!(self.marker, MAGIC, "the marker must be armed throughout the window");
        if let Some(after) = self.reset_after.take() {
            assert!(after < millis);
            self.elapsed += after;
            std::panic::resume_unwind(Box::new("physical Reset"));
        }
        self.elapsed += millis;
    }
}
#[test]
fn first_reset_waits_200ms_and_clears_before_normal_startup() {
    let mut window = Window::default();
    assert!(!bootloader_requested(&mut window));
    assert_eq!(window.elapsed, 200);
    assert_eq!(window.writes, vec![MAGIC, [0; 3]]);
    assert_eq!(window.marker, [0; 3]);
}
#[test]
fn second_reset_during_window_requests_rom_boot_without_another_delay() {
    for at in [0, 1, 37, 199] {
        let mut window = Window { reset_after: Some(at), ..Window::default() };
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bootloader_requested(&mut window)));
        assert!(interrupted.is_err());
        assert_eq!(window.marker, MAGIC);
        assert!(bootloader_requested(&mut window), "second physical Reset at {at}ms must enter ROM USB boot");
        assert_eq!(window.elapsed, at, "a second reset must not wait another window");
        assert_eq!(window.writes, vec![MAGIC, [0; 3]], "clear before the ROM handoff");
        assert_eq!(window.marker, [0; 3]);
    }
}
#[test]
fn expiry_disarms_later_role_recovery_or_software_reboots() {
    let mut window = Window::default();
    for reset in 1..=3 {
        assert!(!bootloader_requested(&mut window));
        assert_eq!(window.elapsed, reset * 200);
        assert_eq!(window.marker, [0; 3]);
    }
}
#[test]
fn rom_handoff_clear_prevents_a_subsequent_boot_loop() {
    let mut window = Window { marker: MAGIC, ..Window::default() };
    assert!(bootloader_requested(&mut window));
    assert_eq!(window.elapsed, 0);
    assert_eq!(window.marker, [0; 3]);
    assert!(!bootloader_requested(&mut window));
    assert_eq!(window.elapsed, 200);
    assert_eq!(window.marker, [0; 3]);
}
#[test]
fn incomplete_or_nonmatching_cold_sram_does_not_request_rom() {
    for changed in 0..3 {
        let mut marker = MAGIC;
        marker[changed] ^= 1;
        let mut window = Window { marker, ..Window::default() };
        assert!(!bootloader_requested(&mut window));
        assert_eq!(window.elapsed, 200);
        assert_eq!(window.marker, [0; 3]);
    }
    for marker in [[0; 3], [u32::MAX; 3], [0xcccccccc; 3]] {
        let mut window = Window { marker, ..Window::default() };
        assert!(!bootloader_requested(&mut window));
        assert_eq!(window.elapsed, 200);
    }
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mutation-controls", action="store_true")
    options = parser.parse_args()
    check_wiring()
    temporary = tempfile.TemporaryDirectory(prefix="phenom-double-reset-")
    directory = Path(temporary.name)
    harness = directory / "test.rs"
    source = HELPER.read_text()
    pin_match = re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT / "rust-toolchain.toml").read_text())
    assert pin_match, "missing canonical Rust toolchain"
    toolchain = os.environ.get("RMK_STABLE_TOOLCHAIN", pin_match.group(1))
    executable = directory / "test"

    def run(production, case=None):
        # Only the hardware adapter is target-gated; the decision is unchanged.
        harness.write_text(production + "\n" + TESTS)
        subprocess.run(["rustup", "run", toolchain, "rustc", "--edition=2021", "-Dwarnings", "--test", str(harness), "-o", str(executable)], check=True)
        command = [str(executable), "--test-threads=1"]
        if case:
            command += ["--exact", case]
        return subprocess.run(command, capture_output=True, text=True)

    result = run(source)
    print(result.stdout + result.stderr, flush=True)
    result.check_returncode()
    if options.mutation_controls:
        controls = [
            ("wrong-window", "const WINDOW_MS: u64 = 200;", "const WINDOW_MS: u64 = 201;", "first_reset_waits_200ms_and_clears_before_normal_startup"),
            ("missing-rom-clear", "        window.write_marker([0; 3]);\n        return true;", "        return true;", "rom_handoff_clear_prevents_a_subsequent_boot_loop"),
            ("missing-expiry-clear", "    window.wait_ms(WINDOW_MS);\n    window.write_marker([0; 3]);", "    window.wait_ms(WINDOW_MS);", "expiry_disarms_later_role_recovery_or_software_reboots"),
            ("partial-token-match", "window.read_marker() == MAGIC", "window.read_marker()[0] == MAGIC[0]", "incomplete_or_nonmatching_cold_sram_does_not_request_rom"),
        ]
        for name, old, new, case in controls:
            assert source.count(old) == 1, f"mutation boundary changed: {name}"
            mutant = source.replace(old, new, 1)
            result = run(mutant, case)
            output = result.stdout + result.stderr
            assert result.returncode == 101 and "panicked at" in output and "1 failed" in output, output
            print(f"mutation {name}: expected assertion RED; sha256 {hashlib.sha256(mutant.encode()).hexdigest()}\n{output}", flush=True)
        result = run(source)
        print("restored production source:\n" + result.stdout + result.stderr, flush=True)
        result.check_returncode()
    for path in (FIRMWARE, HELPER, Path(__file__)):
        print(f"sha256 {hashlib.sha256(path.read_bytes()).hexdigest()} {path.relative_to(ROOT)}")
    temporary.cleanup()


if __name__ == "__main__":
    main()
