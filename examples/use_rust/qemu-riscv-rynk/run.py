from pathlib import Path
import subprocess

PORT = 9000
ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
ELF = HERE / "target/riscv32imac-unknown-none-elf/debug/rmk-qemu-riscv"
# The repository-wide .cargo/config.toml targets nRF52840 firmware, and the
# verifier runs on the host.
HOST = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout.split("host: ")[1].split()[0]

subprocess.run(["cargo", "build"], cwd=HERE, check=True)

q = subprocess.Popen(
    ["qemu-system-riscv32", "-M", "virt", "-cpu", "rv32",
     "-semihosting", "-nographic", "-bios", "none",
     "-kernel", str(ELF), "-serial", f"tcp::{PORT},server,nowait"],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(f"QEMU started, serial on tcp::{PORT}", flush=True)

try:
    subprocess.run(
        [
            "cargo",
            "run",
            "--manifest-path",
            str(ROOT / "rynk/Cargo.toml"),
            "--example",
            "qemu_behavior",
            "--target",
            HOST,
            "--",
            f"127.0.0.1:{PORT}",
        ],
        cwd=ROOT,
        check=True,
    )
finally:
    q.terminate()
    try:
        q.wait(timeout=2)
    except subprocess.TimeoutExpired:
        q.kill()
        q.wait()
