/*
 * GNU ld linker script for NXP LPC845 (Cortex-M0+).
 *
 * Memory map taken from the LPC84x datasheet, section 7.6 ("Memory map"):
 *   - Flash: 0x00000000, 64 KB
 *   - SRAM:  0x10000000, 16 KB (single contiguous bank on LPC845)
 *
 * Reference: https://www.nxp.com/docs/en/data-sheet/LPC84x.pdf
 *
 * Stage 1 skeleton — verified against datasheet, but reviewers should
 * sanity-check section ordering before flashing real hardware.
 */

ENTRY(Reset_Handler)

MEMORY
{
    FLASH (rx)  : ORIGIN = 0x00000000, LENGTH = 64K
    RAM   (rwx) : ORIGIN = 0x10000000, LENGTH = 16K
}

/* Top of RAM — initial stack pointer. */
_estack = ORIGIN(RAM) + LENGTH(RAM);

/*
 * LPC8xx boot-ROM validity check: the first 8 vector words must sum to
 * zero (mod 2^32) or the boot ROM rejects the image as "no valid user
 * code". lpc21isp patches this slot during ISP flashing, but raw SWD
 * programmers (pyocd/jlink/cmsis-dap) do not — so the checksum must be
 * baked into the image itself. Only the linker can sum these relocatable
 * symbols, so it is computed here and emitted at vector offset 0x1C by
 * `.word _isr_vector_checksum` in startup_lpc845.S.
 *
 * The `+ 1` terms add the thumb bit: in linker-script expressions a
 * function symbol resolves to its raw (even) address, whereas the boot
 * ROM sums the table as written, where thumb entries carry bit 0 set
 * (the assembler's `.word Reset_Handler` emits Reset_Handler|1). Reserved
 * vectors at 0x10/0x14/0x18 are zero and drop out of the sum.
 */
_isr_vector_checksum =
    0 - (_estack + (Reset_Handler + 1) + (NMI_Handler + 1) + (HardFault_Handler + 1));

SECTIONS
{
    .isr_vector :
    {
        . = ALIGN(4);
        KEEP(*(.isr_vector))
        . = ALIGN(4);
    } > FLASH

    .text :
    {
        . = ALIGN(4);
        *(.text)
        *(.text*)
        *(.rodata)
        *(.rodata*)
        *(.glue_7)
        *(.glue_7t)
        *(.eh_frame)
        KEEP (*(.init))
        KEEP (*(.fini))
        . = ALIGN(4);
        _etext = .;
    } > FLASH

    .ARM.extab : { *(.ARM.extab* .gnu.linkonce.armextab.*) } > FLASH
    .ARM : { __exidx_start = .; *(.ARM.exidx*) __exidx_end = .; } > FLASH

    .preinit_array :
    {
        PROVIDE_HIDDEN (__preinit_array_start = .);
        KEEP (*(.preinit_array*))
        PROVIDE_HIDDEN (__preinit_array_end = .);
    } > FLASH

    .init_array :
    {
        PROVIDE_HIDDEN (__init_array_start = .);
        KEEP (*(SORT(.init_array.*)))
        KEEP (*(.init_array*))
        PROVIDE_HIDDEN (__init_array_end = .);
    } > FLASH

    .fini_array :
    {
        PROVIDE_HIDDEN (__fini_array_start = .);
        KEEP (*(SORT(.fini_array.*)))
        KEEP (*(.fini_array*))
        PROVIDE_HIDDEN (__fini_array_end = .);
    } > FLASH

    _sidata = LOADADDR(.data);

    .data :
    {
        . = ALIGN(4);
        _sdata = .;
        *(.data)
        *(.data*)
        . = ALIGN(4);
        _edata = .;
    } > RAM AT > FLASH

    .bss :
    {
        . = ALIGN(4);
        _sbss = .;
        __bss_start__ = _sbss;
        *(.bss)
        *(.bss*)
        *(COMMON)
        . = ALIGN(4);
        _ebss = .;
        __bss_end__ = _ebss;
    } > RAM

    /DISCARD/ :
    {
        libc.a ( * )
        libm.a ( * )
        libgcc.a ( * )
    }

    .ARM.attributes 0 : { *(.ARM.attributes) }
}
