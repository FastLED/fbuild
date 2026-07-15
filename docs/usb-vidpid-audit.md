# USB VID/PID source audit

Audit for FastLED/fbuild#1047, performed against `origin/main` on 2026-07-14.
The authoritative board/device identity source is the published
[FastLED/boards registry](https://github.com/FastLED/boards). fbuild consumes
the published `usb-vids.proto.zstd` artifact through
`crates/fbuild-core/src/usb/data.rs`; the release build no longer embeds a
catalogue blob (`EMBEDDED_PROTO` is empty outside tests).

## Occurrence inventory

| Location | Classification | Disposition/provenance |
| --- | --- | --- |
| `crates/fbuild-config/assets/boards/json/*.json` (1,645 checked-in board records; 516 contain `build.vid` and/or `build.pid`) | Canonical board metadata snapshot | These fields are copied from the FastLED/boards publication and are the only board-level identity input. The snapshot must be refreshed from FastLED/boards; fbuild must not add guessed values. |
| `crates/fbuild-config/src/board/mcu_vid.rs` + `online-data-tools/seed_mcu_to_vid.json` | Legacy MCU-family heuristic | `mcu_vid.rs` embeds the seed at compile time and maps MCU names to a guessed VID when board metadata is absent. This is production identity knowledge, not a test fixture; it requires FastLED/boards#47 to publish an explicit semantic identity/role field before removal. |
| `crates/fbuild-core/src/usb/data.rs` (`MANIFEST_URL`, protobuf/JSON cache boundary) | Canonical ingestion | Runtime cache populated from FastLED/boards; legacy JSON URL remains compatibility-only and must be removed after consumers migrate. |
| `crates/fbuild-core/data/usb-vendors.tar.zst` + `crates/fbuild-core/src/usb/embedded.rs` + `crates/fbuild-core/src/usb/resolver.rs` | Production vendor-only fallback | This archive is compile-time embedded in every release and `resolve_bundled()` falls back to it for vendor names. It is not a board-role catalogue, but it is still production USB identity data and must remain explicitly vendor-only; product/role resolution must come from FastLED/boards. |
| `crates/fbuild-serial/src/boards.rs` (`BOARD_FINGERPRINTS`, `ENVIRONMENT_TO_VCOM`, `family_for_vid_pid`) | Legacy runtime catalogue | Unsafe duplicate of board identity data. Existing rows are retained for compatibility in this PR and are the migration blocker: each row needs a matching FastLED/boards record plus a schema field expressing deploy family/reset semantics before removal. |
| `crates/fbuild-serial/src/bootloader_watcher.rs` | Legacy bootloader VID/PID signatures | RP2040/SAMD/Teensy bootloader detection still uses concrete signatures; boards metadata needs a bootloader identity/role field before this can become data-driven. |
| `crates/fbuild-daemon/src/handlers/operations/deploy_port.rs` | Legacy runtime VID fallback | Expected vendor IDs are deploy-port selection heuristics, not names; they still duplicate identity knowledge and require boards-derived upload metadata before removal. |
| `crates/fbuild-deploy/src/lpc_debugger_reflash.rs` | Protocol/device compatibility constants | LPC-Link2 firmware recovery requires the exact probe identity. Provenance is NXP/FastLED LPC-Link2 documentation; move to boards metadata when the probe schema supports non-board recovery targets. |
| `crates/fbuild-deploy/src/probe_rs.rs`, `crates/fbuild-deploy/src/teensy/port_discovery.rs` | Legacy runtime probe/loader matching | Probe and HalfKay discovery have explicit VID/PID signatures; these need published role records or a protocol-level classifier before removal. |
| `ci/validate_boards.py` | CI validation fixtures | PlatformIO cross-check exceptions are CI-only and do not ship in fbuild. Each exception carries an issue/provenance comment. |
| `crates/**/tests`, `ci/docker-test-serial`, and test-support fixtures | Test-only fixtures | Concrete IDs are intentionally isolated from production and must not be imported by runtime modules. Test-only paths are outside this preparatory diff guard; final #1047 cleanup will define the deny-all fixture policy. |
| `online-data-tools/**` | Legacy data pipeline | Fetchers/builders are offline maintenance tooling, not a runtime dependency. They remain deprecated until the FastLED/boards publication pipeline fully replaces their outputs. |
| `ids.json`, `ids2.json`, `ids3.json`, `ids4.json` + `online-data-tools/vendor_names_inlined.py` | Legacy vendor-name inputs | Scraped/triaged vendor-name lists used to build the embedded vendor archive; they are not board-role records, but remain active production-artifact inputs through the nightly workflow. |
| `.github/workflows/update-data.yml` and `online-data-tools/update_www.py` | Active publication workflow | Fetches vendor/PID supplements, merges them into online-data, emits `usb-vendors.tar.zst` and `usb-vids.proto.zstd`, then publishes orphan branches. The workflow is active legacy infrastructure and must be reconciled with FastLED/boards#47 rather than silently treated as test-only. |

Non-VID/PID hexadecimal values (memory addresses, protocol magic numbers,
Windows flags, and UUID fields) are not USB identities and are outside this
audit.

## Concrete production pairs (current main)

The following is the deduplicated pair inventory from the production paths
above (test-only assertions are intentionally omitted):

| Path | Pairs |
| --- | --- |
| `crates/fbuild-serial/src/boards.rs` | `1FC9:0132`, `16C0:0483`, `303A:1001`, `303A:0002`, `10C4:EA60`, `10C4:EA70`, `1A86:7523`, `1A86:55D4`, `0403:6001`, `0403:6015`, `2341:0043`, `2341:0001`, `2341:0010`, `2341:804E`, `2E8A:000A`, `2E8A:0003` |
| `crates/fbuild-serial/src/bootloader_watcher.rs` | `2E8A:0003`, `03EB:6124`, `239A:*`, `16C0:0478` |
| `crates/fbuild-daemon/src/handlers/operations/deploy_port.rs` | `16C0:*`, `303A:*`, `2341:*`, `2A03:*`, `1A86:*`, `10C4:*`, `0403:*`, `1FC9:*`, `0D28:*`, plus test-only concrete rows |
| `crates/fbuild-deploy/src/lpc_debugger_reflash.rs` | `1FC9:0132` |
| `crates/fbuild-deploy/src/probe_rs.rs` | `1FC9:0090`, `1FC9:0132` |
| `crates/fbuild-deploy/src/teensy/port_discovery.rs` | `16C0:*` |

The wildcard rows are vendor-family fallbacks, not claims that every PID is
valid for the named board. They are still production identity knowledge and
remain migration blockers.

## Required boards records before catalogue removal

The following existing runtime rows need provenance-backed records in
FastLED/boards before they can be deleted: Raspberry Pi `2e8a` runtime and
BOOTSEL identities, Espressif native USB `303a` rows, NXP/LPC `1fc9` and
`16c0:0483`, Arduino `2341`, CP210x/CH340/FTDI bridge identities (`10c4`,
`1a86`, `0403`), and the LPC-Link2 recovery probe (`1fc9:0132`). This PR does
not guess or add those records; it installs the guard that prevents the list
from growing and records the exact migration dependency.

The board snapshot and MCU heuristic add a second migration surface: the 516
JSON records carrying `build.vid`/`build.pid`, plus every row in
`seed_mcu_to_vid.json`, need publication provenance and an explicit distinction
between runtime CDC, bootloader, debug-probe, and bridge roles. FastLED/boards
issue #47 is the dependency for that schema/record work.

This audit inventories source families and counts; it does not enumerate every
one of the 516 board rows. Row-level provenance and the broader deny-all
cleanup are final #1047 work after FastLED/boards#47 lands.

## Guard policy

`ci/check_usb_vidpid_literals.py` is a preparatory diff guard. It scans added
same-line hexadecimal/string pairs in `crates/` and `python/`, and separate
`"vid"`/`"pid"` fields only in
`crates/fbuild-config/assets/boards/json/`. Existing compatibility rows are
allowed because this is diff-based. It does **not** yet catch separate Rust
constants, decimal or symbolic forms, non-board JSON, CI/setup/workflow/root
paths, or enumerate each of the 516 board rows. Those final #1047 cleanup and
deny-all checks are deferred until FastLED/boards#47 lands.
