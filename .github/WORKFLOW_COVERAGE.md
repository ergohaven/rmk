# RMK test builds and release qualification

## Ordinary test firmware

Run **Build Ergohaven RMK Firmware** (`build.yml`) manually on the intended
branch. Only the existing standalone and Qube matrices run: compile the
matching physical halves (and Qube dongle), convert to UF2, upload the
per-profile artifacts. The production inventory is seven standalone and
seven Qube profiles, all RMK/Rynk. No host/native tests, migration executables,
storage-reset utilities, simulator, example matrix or policy API lookup runs
in an ordinary manual firmware build.

Phenom, Phenom Mini and Phenom Micro temporarily ship QMK/Vial instead. Their
RMK rows are excluded from both ordinary and release firmware builds. Sources,
manual build configurations and descriptor/feature coverage are retained.

The manual matrices do not depend on the contract-test planner. A failed
lightweight configuration check therefore cannot silently turn an explicit
artifact request into a complete qualification campaign. Compilation failures
still fail the affected matrix job; fail-fast remains disabled.

## Automatic changes and releases

Main pushes and PRs execute the lightweight workflow/policy contracts and
completion gates only. `ci.yml` no longer has a manual full-test dispatch.
Full test and firmware qualification runs on `v*` tags, or when the existing
Publish gate calls both workflows with `full-validation: true`. Reusable calls
inherit their caller's event: this explicit boolean, not an assumed
`workflow_call` event name, selects the full graph.

Release qualification retains all native/feature/macro tests, format and
lockfile checks, host/Wasm, QEMU, discovered stable/ESP examples, firmware
profiles, migration preflight and storage utilities. Existing owner payload
preservation digests remain unchanged. The Publish job waits for both full
profiles before the existing approval/OIDC/secret-protected publishing steps.
No signing, identity, approval or secret boundary is relaxed. The separate
user-config build and upstream release-PR automation are unchanged.

The two reusable owners have distinct concurrency prefixes, so calls from one
Publish workflow cannot cancel one another. Completion/proof jobs remain
fail-closed; an ordinary artifact-only dispatch skips the full firmware proof
rather than advertising partial coverage as release qualification.

## Lightweight verification

With `PyYAML==6.0.3` installed:

```sh
python3 -m unittest discover -s .github/scripts -p 'test_*.py'
git diff --check
```

These checks validate workflow routing, preserved payloads, actual shell
selection/completion boundaries and production descriptor contracts without
compiling firmware. They do not establish hosted build success or hardware
acceptance. The immutable historical geometry/profile fixtures are retained;
a separate source-pinned successor covers the already accepted Phenom module
editor bounds and thumb geometry.
