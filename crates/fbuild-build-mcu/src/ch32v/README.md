# CH32V Platform Build Support

Build orchestrator for WCH CH32V RISC-V MCUs. Uses xPack riscv-none-elf-gcc toolchain and OpenWCH Arduino core framework.

The V303/V307 board definitions use the vendor hard-float profile
(`rv32imafcxw`/`ilp32f`, normalized to `rv32imafc_zicsr`). This intentionally
differs from Community-PIO-CH32V's soft-float defaults. The vendor builder also
uses `-msave-restore -msmall-data-limit=8 -fno-use-cxa-atexit`; fbuild keeps
`-msmall-data-limit=0` and omits those flags, so reference-build flash-size
comparisons should record that code-generation difference.

Some board packages reuse the nearest upstream pin-map variant because the
pinned OpenWCH core does not ship a dedicated map: V208 uses the V203 map,
X035 uses the G8U map, and V103 uses the R8T6 map. These are intentional
registry fallbacks, not claims that the physical pinouts are identical.
