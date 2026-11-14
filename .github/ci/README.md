# CI toolchain maintenance

## Stable baseline

The root `rust-toolchain.toml` owns the exact stable release used by core and
host CI. `.github/ci/_lib.sh` exports it as `RMK_STABLE_TOOLCHAIN`; `check.sh`,
`test.sh` and `host.sh` use that value for explicit Cargo and rustc selectors.
The four corresponding installers in `ci.yml` read the same bootstrap output.
Keep the allowed `dtolnay/rust-toolchain@stable` action reference and select the
compiler through its `toolchain` input, not a new versioned action reference.
Bare Cargo commands in the QEMU verifier inherit the root toolchain through
rustup's directory lookup. Do not add a nested toolchain or an environment
override to bypass that baseline.

The current baseline is Rust **1.99.1** and the macro-expansion tool is
**cargo-expand 1.0.127**, installed with `--locked`. This pair passed all owners
in [full CI 43225011172](https://github.com/ergohaven/rmk/actions/runs/43225011172).
Floating stable advanced to 1.100.0 in
[failed CI 43633064653](https://github.com/ergohaven/rmk/actions/runs/43633064653):
Clippy added a passkey lint, derive expansion spelling changed, and inherited
`bluer 0.16.1` unit-return inference became an error. The baseline avoids changing
firmware behavior, dependency resolutions or strict expansion snapshots solely
for this release repair. Historical success is not validation of a new source.

## Update policy

This is a maintained baseline, not an indefinite freeze. At each stable Rust
release, and before the next RMK release, review the baseline for compatibility
and security/support needs. Propose any compiler, cargo-expand, dependency or
snapshot migration explicitly; do not float the installer or silence a failing
owner to obtain green CI.

1. Change the root channel once to the proposed exact release. All stable
   host/core selectors and installers must follow it without extra version
   strings. Update the cargo-expand pin only when its output is characterized.
2. Run `python3 -m unittest discover -s .github/scripts -v` with Python 3.11+
   and PyYAML (CI installs its pinned version). The toolchain tests execute real
   shell owners with inert command recorders; they do not compile Rust or prove
   firmware, host, wasm or QEMU behavior.
3. Run `.github/ci/format.sh` with the installed nightly formatter. Do not alter
   the separate nightly, ESP or board-specific toolchain policies as a side
   effect. Respect local build-cache budgets; use the full hosted graph when
   native compilation is blocked locally.
4. Review every feature row, assertion/filter and workflow condition before
   refreshing any command-inventory or workflow-payload hash. Retain mutation
   rejection of floating/mismatched selectors and removed coverage. Never
   regenerate the five macro goldens blindly; review compiler-only output
   differences independently from generator/API changes.
5. Require the final exact-source **full CI graph**, including strict macro
   expansion/compile-fail, every behavior feature row, Clippy, host/wasm and the
   QEMU behavior verifier, to pass. A firmware-only build or an old green source
   cannot satisfy that gate. Require the separate complete firmware graph and
   verify all official UF2 outputs and release versions before publication.

For dependency compatibility, track the upstream explicit-unit inference fix
and a deliberate bluest/bluer upgrade separately; changing a compiler baseline
must not silently replace the production BLE API or lockfiles.
