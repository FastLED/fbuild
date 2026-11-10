# RP2040

RP2040/RP2350 platform build support (Raspberry Pi Pico).

Uses the generic ARM compiler/linker from `generic_arm/` with RP2040-specific
MCU configurations and the RP2040 Arduino cores framework.

`board_props.rs` applies Arduino-Pico board properties and resolves the variant
from the selected framework while preserving explicit project overrides.

`core_select.rs` selects the framework family without silently substituting an
SDK (FastLED/fbuild#1977). Stock `pico`/`nanorp2040connect` select Arduino Mbed;
`board_build.core = earlephilhower` explicitly selects Arduino-Pico. Unknown
explicit core names fail at the boundary.

`mbed.rs` builds the Mbed wrapper core and links the selected variant's
`libmbed.a` using its published compile, define, include, and linker inputs.
The SDK include recipe retains `-iprefix`/`-iwithprefixbefore` to avoid expanding
hundreds of long SDK paths into GCC's argument environment. Mbed and Arduino-Pico
use distinct core layouts and compiler inputs. Successful links produce an ELF
and a managed-picotool UF2; board deployment remains owned by `fbuild deploy`.
The ignored real-build acceptance case covers both explicit Arduino-Pico and
stock-Pico Mbed selection. This does not promise every consumer library supports
both cores or that physical hardware output was validated.
