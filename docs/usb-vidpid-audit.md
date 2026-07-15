# USB VID/PID source audit

Audit for FastLED/fbuild#1047, updated 2026-07-14. The authoritative
board/device identity source is the published
[FastLED/boards registry](https://github.com/FastLED/boards). Production
fbuild code may consume its verified runtime cache, but must not ship a copied
device catalogue or board-specific USB constants.

## Production consumers

| Location | Current disposition |
| --- | --- |
| `crates/fbuild-core/src/usb/profiles.rs` | Verifies the published schema and SHA-256, validates typed identities and provenance, and indexes board aliases, roles, transports, generations, and primary compile identities. |
| `crates/fbuild-core/src/usb/data.rs` | Downloads the FastLED/boards display catalogue. Its embedded protobuf exists only under `cfg(test)`. |
| `crates/fbuild-config/assets/boards/json/*.json` | Contains no `build.vid` or `build.pid` fields. These 1,645 build-metadata snapshots are not a USB identity source. |
| `crates/fbuild-config/src/board` | Resolves firmware USB defines from a verified board profile's `primary_compile_identity`. Explicit project-local overrides remain supported; the former MCU-to-VID heuristic is removed. |
| `crates/fbuild-serial/src/boards.rs` | Production hints, VCOM selection, and reset-family classification derive from typed profiles. Concrete lookup data is test-only. |
| `crates/fbuild-serial/src/bootloader_watcher.rs` | Production bootloader detection uses typed purpose, role, family, and transport data. Concrete signatures are test-only. |
| `crates/fbuild-daemon/src/handlers/operations/deploy_port.rs` | Automatic selection uses board membership and typed runtime profiles. Missing identity data fails closed. |
| `crates/fbuild-deploy` probe, LPC, Teensy, and RP paths | Production selection uses typed profiles or caller-supplied selectors. Concrete selectors are confined to tests. |

FastLED/boards PRs #48-#54 supplied the typed schema, aliases, role records,
publication support, deterministic primary compile identity, and Arduino-Pico
`USBD_VID`/`USBD_PID` extraction needed for this migration.

## Test-only fixtures

Concrete USB identities remain allowed in tests. This includes `#[cfg(test)]`
Rust modules, `tests/` trees, CI hardware fixtures, and the archives under
`crates/fbuild-core/data/` that are included only in test builds. Production
modules must not import or enable these fixtures.

## Remaining legacy publication surface

The following fbuild-owned data-publishing infrastructure is not used as a
production runtime fallback, but still duplicates USB identity collection and
publication that now belongs in FastLED/boards:

- `.github/workflows/update-data.yml`;
- `online-data-tools/**`, including `seed_mcu_to_vid.json`;
- root `ids.json`, `ids2.json`, `ids3.json`, and `ids4.json`;
- the `dump_usb_ids` maintenance example and old online-data documentation.

It must be removed after confirming that no release, cache, or documentation
consumer still points at the retired fbuild branches. FastLED/boards owns all
future source collection and publication.

## Guard status

`ci/check_usb_vidpid_literals.py` currently prevents new same-line production
pairs and bundled-board `vid`/`pid` fields. Final #1047 work must replace this
preparatory diff check with a full-tree deny rule that understands Rust
`cfg(test)` boundaries, permits explicit test fixtures, and rejects production
catalogues, generated tables, board snapshots, and workflow-owned identity
sources.

Non-USB hexadecimal values such as memory addresses, protocol magic numbers,
Windows flags, and UUID fields are outside this policy.
