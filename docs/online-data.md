# USB identity data

FastLED/boards is the only production source for USB VID/PID identities used
by fbuild. fbuild consumes the published, verified artifacts at runtime; it
does not collect, generate, embed, or publish a second catalogue.

## Published artifacts

- Metadata and hashes: `https://fastled.github.io/boards/_meta.json`
- Typed board profiles: `https://fastled.github.io/boards/usb-profiles.json`
- Display-name catalogue: `https://fastled.github.io/boards/usb-ids.json`
- Compact display-name catalogue:
  `https://fastled.github.io/boards/usb-vids.proto.zstd`

`fbuild_core::usb::profiles` verifies the typed profile artifact against the
metadata document before installing it. `fbuild_core::usb::data` manages the
display-name cache. Failed downloads or validation never authorize a bundled
production fallback: identity-dependent behavior fails closed, while display
labels degrade to deterministic `Unknown` text.

The files cached beneath fbuild's shared `usb/` cache directory are disposable
copies of these published artifacts. They are not an additional source of
truth and must not be edited or checked into this repository.

## Ownership rule

New boards, aliases, bootloader identities, runtime identities, compile-time
identities, transports, and VID/PID corrections belong in
[FastLED/boards](https://github.com/FastLED/boards). fbuild changes should
consume the resulting schema rather than adding device constants.

Concrete identities may exist only in explicit tests and fixtures. The frozen
archives under `crates/fbuild-core/data/` are test inputs and are excluded from
release/runtime builds. `ci/check_usb_vidpid_literals.py` enforces this boundary
over the full tracked tree.

## Retired publisher

The former fbuild-owned `online-data` branch, refresh workflow, root `ids*.json`
files, and `online-data-tools/` pipeline are retired. They must not be restored
or used as runtime fallbacks. All future collection and publishing work belongs
to FastLED/boards.
