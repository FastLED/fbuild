/*
 * Startup / vector table for NXP LPC845 (Cortex-M0+).
 *
 * Provides:
 *   - Initial stack pointer
 *   - Reset_Handler (copies .data, zeros .bss, calls SystemInit then main)
 *   - Expanded vector table covering ARMv6-M system vectors + LPC845
 *     peripheral IRQs (see UM11029 table 5 "Interrupt sources")
 *   - SystemInit that brings core to 24 MHz from the on-chip FRO (FRO direct),
 *     with flash wait states programmed per UM11029 section 4.5 (FMC FLASHCFG)
 *
 * All non-Reset vectors alias Default_Handler (weak). Drivers may override
 * any IRQ by defining the corresponding `<Name>_IRQHandler` symbol.
 *
 * References:
 *   - UM11029 (LPC84x User Manual), section 4 (system config), section 4.5
 *     (FMC FLASHCFG), table 5 (interrupt sources)
 *     https://www.nxp.com/docs/en/data-sheet/LPC84x.pdf
 *   - NXP CMSIS device header `LPC845.h` (register layout)
 *
 * Tracking: FastLED/FastLED#2845 (Stage 3 — port completion).
 */

    .syntax unified
    .cpu cortex-m0plus
    .thumb

    .global g_pfnVectors
    .global Default_Handler
    .global Reset_Handler
    .global SystemInit

    .extern _estack
    .extern _sidata
    .extern _sdata
    .extern _edata
    .extern _sbss
    .extern _ebss
    .extern main
    /* Boot-ROM vector checksum, computed by the linker script (see lpc845.ld). */
    .extern _isr_vector_checksum

/* ---------------------------------------------------------------------------
 * Vector table — placed in flash at 0x00000000 via .isr_vector.
 *
 * Layout: 16 ARMv6-M system entries + LPC845 peripheral IRQs
 * (UM11029 table 5). Total = 16 + 32 = 48 entries for the peripheral
 * range covered here. Vectors 16..47 correspond to IRQ#0..IRQ#31 in the
 * NVIC view; SPI0..PIN_INT7 below match the IRQ numbers in `LPC845.h`.
 * --------------------------------------------------------------------------- */
    .section .isr_vector, "a", %progbits
    .type g_pfnVectors, %object

g_pfnVectors:
    /* ARMv6-M system vectors (0..15). */
    .word _estack              /* 0x00 Initial stack pointer */
    .word Reset_Handler        /* 0x04 Reset */
    .word NMI_Handler          /* 0x08 NMI */
    .word HardFault_Handler    /* 0x0C HardFault */
    .word 0                    /* 0x10 reserved */
    .word 0                    /* 0x14 reserved */
    .word 0                    /* 0x18 reserved */
    .word _isr_vector_checksum /* 0x1C boot-ROM checksum (vectors 0..7 sum to 0) */
    .word 0                    /* 0x20 reserved */
    .word 0                    /* 0x24 reserved */
    .word 0                    /* 0x28 reserved */
    .word SVC_Handler          /* 0x2C SVCall */
    .word 0                    /* 0x30 reserved */
    .word 0                    /* 0x34 reserved */
    .word PendSV_Handler       /* 0x38 PendSV */
    .word SysTick_Handler      /* 0x3C SysTick */

    /* LPC845 peripheral IRQs (UM11029 table 5).
     * IRQ numbers per CMSIS device header `LPC845.h`. */
    .word SPI0_IRQHandler        /* IRQ  0 — SPI0 */
    .word SPI1_IRQHandler        /* IRQ  1 — SPI1 */
    .word DAC0_IRQHandler        /* IRQ  2 — DAC0 */
    .word USART0_IRQHandler      /* IRQ  3 — USART0 */
    .word USART1_IRQHandler      /* IRQ  4 — USART1 */
    .word USART2_IRQHandler      /* IRQ  5 — USART2 */
    .word 0                      /* IRQ  6 — reserved */
    .word I2C1_IRQHandler        /* IRQ  7 — I2C1 */
    .word I2C0_IRQHandler        /* IRQ  8 — I2C0 */
    .word SCT_IRQHandler         /* IRQ  9 — SCT (state-configurable timer) */
    .word MRT_IRQHandler         /* IRQ 10 — Multi-rate timer */
    .word CMP_IRQHandler         /* IRQ 11 — Analog comparator / CAPT */
    .word WDT_IRQHandler         /* IRQ 12 — Windowed watchdog */
    .word BOD_IRQHandler         /* IRQ 13 — Brown-out detect */
    .word FLASH_IRQHandler       /* IRQ 14 — Flash controller */
    .word WKT_IRQHandler         /* IRQ 15 — Wake-up timer */
    .word ADC_SEQA_IRQHandler    /* IRQ 16 — ADC sequence A complete */
    .word ADC_SEQB_IRQHandler    /* IRQ 17 — ADC sequence B complete */
    .word ADC_THCMP_IRQHandler   /* IRQ 18 — ADC threshold compare */
    .word ADC_OVR_IRQHandler     /* IRQ 19 — ADC overrun */
    .word DMA_IRQHandler         /* IRQ 20 — DMA0 (single controller) */
    .word I2C2_IRQHandler        /* IRQ 21 — I2C2 */
    .word I2C3_IRQHandler        /* IRQ 22 — I2C3 */
    .word CTIMER0_IRQHandler     /* IRQ 23 — Standard counter/timer CTIMER0 */
    .word PIN_INT0_IRQHandler    /* IRQ 24 — Pin interrupt 0 */
    .word PIN_INT1_IRQHandler    /* IRQ 25 — Pin interrupt 1 */
    .word PIN_INT2_IRQHandler    /* IRQ 26 — Pin interrupt 2 */
    .word PIN_INT3_IRQHandler    /* IRQ 27 — Pin interrupt 3 */
    .word PIN_INT4_IRQHandler    /* IRQ 28 — Pin interrupt 4 */
    .word PIN_INT5_IRQHandler    /* IRQ 29 — Pin interrupt 5 / DAC1 */
    .word PIN_INT6_IRQHandler    /* IRQ 30 — Pin interrupt 6 / USART3 */
    .word PIN_INT7_IRQHandler    /* IRQ 31 — Pin interrupt 7 / USART4 */

    .size g_pfnVectors, . - g_pfnVectors

/* ---------------------------------------------------------------------------
 * Reset handler.
 * --------------------------------------------------------------------------- */
    .text
    .thumb_func
    .type Reset_Handler, %function
Reset_Handler:
    /* Copy .data from FLASH (LMA) to RAM (VMA). */
    ldr     r0, =_sdata
    ldr     r1, =_edata
    ldr     r2, =_sidata
    movs    r3, #0
1:
    cmp     r0, r1
    bcs     2f
    ldr     r4, [r2, r3]
    str     r4, [r0, r3]
    adds    r3, #4
    adds    r0, #4
    b       1b
2:
    /* Zero .bss. */
    ldr     r0, =_sbss
    ldr     r1, =_ebss
    movs    r2, #0
3:
    cmp     r0, r1
    bcs     4f
    str     r2, [r0]
    adds    r0, #4
    b       3b
4:
    /* SystemInit configures clocks + flash wait states. */
    bl      SystemInit
    /*
     * Run C++ static/global constructors (.init_array). Without this the
     * vtable pointers of polymorphic globals (e.g. fl's default
     * memory_resource singleton) are left as uninitialised RAM, so the first
     * virtual call through one — the first heap allocation on the JSON-RPC
     * path — dereferences a garbage vtable and HardFaults. newlib provides
     * __libc_init_array; the .init_array section is emitted by the linker
     * script. (Serial/FL_WARN survive without it because they touch no
     * constructor-initialised global.)
     */
    bl      __libc_init_array
    bl      main
LoopForever:
    b       LoopForever
    .size Reset_Handler, . - Reset_Handler

/* ---------------------------------------------------------------------------
 * Default IRQ handler — infinite loop.
 * --------------------------------------------------------------------------- */
    .thumb_func
    .type Default_Handler, %function
Default_Handler:
    b       Default_Handler
    .size Default_Handler, . - Default_Handler

/* ---------------------------------------------------------------------------
 * _init / _fini — no-op stubs.
 *
 * newlib's __libc_init_array (which Reset_Handler now calls) ends with a call
 * to _init; the matching __libc_fini_array calls _fini. These are normally
 * supplied by crti.o/crtn.o, which a -nostartfiles bare-metal link does not
 * pull in, leaving an undefined reference. We run constructors purely via the
 * .init_array section, so empty stubs are sufficient.
 * --------------------------------------------------------------------------- */
    .thumb_func
    .weak _init
    .type _init, %function
_init:
    bx      lr
    .size _init, . - _init

    .thumb_func
    .weak _fini
    .type _fini, %function
_fini:
    bx      lr
    .size _fini, . - _fini

/* ---------------------------------------------------------------------------
 * SystemInit — clock + flash setup for LPC845 @ 24 MHz from FRO (FRO direct).
 *
 * UM11029 section 4 procedure (FRO direct, no PLL needed up to 30 MHz):
 *   1. Configure flash wait states in FMC->FLASHCFG before raising the
 *      core clock (section 4.5). LPC845 flash supports:
 *        - 0 wait states up to 20 MHz
 *        - 1 wait state up to 30 MHz
 *        - 2 wait states up to 60 MHz (LPC845 max is 30 MHz, so 1 WS suffices)
 *      FLASHCFG.FLASHTIM (bits[1:0]): 0=1 cycle, 1=2 cycles, 2=3 cycles.
 *      "FLASHTIM = 1" = 1 wait state, valid for the 24 MHz core clock here.
 *      Other bits in FLASHCFG must be preserved.
 *   2. The FRO post-divider (/2) leaves the core at 12 MHz on reset. Setting
 *      FROOSCCTRL.DIRECT (bit 17) bypasses the divider so the core runs the
 *      undivided 24 MHz FRO output, which divides accurately to 115200 baud.
 *
 * Validated on LPC845-BRK hardware (FastLED/FastLED#2845, fbuild#456):
 *   FRO direct at 24 MHz produces clean 115200 UART and correct SysTick
 *   timing. F_CPU consumers must use 24000000UL to match. The boot ROM API
 *   is not required — the direct register sequence below is sufficient.
 *
 * Register addresses (UM11029, CMSIS LPC845.h):
 *   SYSCON base   = 0x40048000
 *     FROOSCCTRL      @ 0x028, FRODIRECTCLKUEN @ 0x030
 *     SYSAHBCLKDIV    @ 0x078
 *     MAINCLKSEL      @ 0x070, MAINCLKUEN @ 0x074
 *   FMC base      = 0x40040000
 *     FLASHCFG     @ 0x010
 * --------------------------------------------------------------------------- */
    .equ LPC845_FMC_BASE,         0x40040000
    .equ LPC845_FMC_FLASHCFG_OFS, 0x010
    .equ LPC845_SYSCON_BASE,      0x40048000
    .equ LPC845_SYSCON_FROOSCCTRL_OFS,      0x028
    .equ LPC845_SYSCON_FRODIRECTCLKUEN_OFS, 0x030
    .equ LPC845_SYSCON_MAINCLKSEL_OFS,    0x070
    .equ LPC845_SYSCON_MAINCLKUEN_OFS,    0x074
    .equ LPC845_SYSCON_SYSAHBCLKDIV_OFS,  0x078

    /* FLASHTIM[1:0] = 1 → 1 wait state (covers up to 30 MHz core clock). */
    .equ LPC845_FLASHCFG_1WS,     0x1
    .equ LPC845_FLASHCFG_TIM_MASK, 0x3

    .weak SystemInit
    .thumb_func
    .type SystemInit, %function
SystemInit:
    /* (1) Program flash wait states BEFORE raising the core clock.
     *     FLASHCFG = (FLASHCFG & ~0x3) | 1   — set 1 wait state. */
    ldr     r0, =LPC845_FMC_BASE
    ldr     r1, [r0, #LPC845_FMC_FLASHCFG_OFS]
    movs    r2, #LPC845_FLASHCFG_TIM_MASK
    bics    r1, r2
    movs    r2, #LPC845_FLASHCFG_1WS
    orrs    r1, r2
    str     r1, [r0, #LPC845_FMC_FLASHCFG_OFS]

    /* (2) Ensure SYSAHBCLKDIV = 1 (reset default, but make it explicit). */
    ldr     r0, =LPC845_SYSCON_BASE
    movs    r1, #1
    str     r1, [r0, #LPC845_SYSCON_SYSAHBCLKDIV_OFS]

    /* (3) MAINCLKSEL = 0 (FRO direct). Latch via MAINCLKUEN 0→1.
     *     UM11029 §4.6.3: software must toggle MAINCLKUEN to update. */
    movs    r1, #0
    str     r1, [r0, #LPC845_SYSCON_MAINCLKSEL_OFS]
    str     r1, [r0, #LPC845_SYSCON_MAINCLKUEN_OFS]
    movs    r1, #1
    str     r1, [r0, #LPC845_SYSCON_MAINCLKUEN_OFS]

    /* (4) FRO direct: bypass the /2 post-divider so the core runs the
     *     undivided 24 MHz FRO output (FROOSCCTRL bit 17 = DIRECT). The reset
     *     default leaves the core at 12 MHz, which cannot divide to an accurate
     *     115200 baud; 24 MHz divides at 0.16% error. Validated on LPC845-BRK
     *     hardware. Latch via FRODIRECTCLKUEN 0->1 (UM11029 §4.6: software must
     *     toggle the UEN register to apply the update). r0 still holds
     *     SYSCON_BASE from step (2). */
    ldr     r1, [r0, #LPC845_SYSCON_FROOSCCTRL_OFS]
    movs    r2, #1
    lsls    r2, r2, #17
    orrs    r1, r2
    str     r1, [r0, #LPC845_SYSCON_FROOSCCTRL_OFS]
    movs    r1, #0
    str     r1, [r0, #LPC845_SYSCON_FRODIRECTCLKUEN_OFS]
    movs    r1, #1
    str     r1, [r0, #LPC845_SYSCON_FRODIRECTCLKUEN_OFS]

    bx      lr
    .size SystemInit, . - SystemInit

/* ---------------------------------------------------------------------------
 * Weak aliases for all IRQs — each one points at Default_Handler unless
 * a driver provides a strong definition.
 * --------------------------------------------------------------------------- */
    .macro def_irq_handler handler_name
    .weak \handler_name
    .thumb_set \handler_name, Default_Handler
    .endm

    /* System vectors. */
    def_irq_handler NMI_Handler
    def_irq_handler HardFault_Handler
    def_irq_handler SVC_Handler
    def_irq_handler PendSV_Handler
    def_irq_handler SysTick_Handler

    /* Peripheral IRQs (UM11029 table 5). */
    def_irq_handler SPI0_IRQHandler
    def_irq_handler SPI1_IRQHandler
    def_irq_handler DAC0_IRQHandler
    def_irq_handler USART0_IRQHandler
    def_irq_handler USART1_IRQHandler
    def_irq_handler USART2_IRQHandler
    def_irq_handler I2C1_IRQHandler
    def_irq_handler I2C0_IRQHandler
    def_irq_handler SCT_IRQHandler
    def_irq_handler MRT_IRQHandler
    def_irq_handler CMP_IRQHandler
    def_irq_handler WDT_IRQHandler
    def_irq_handler BOD_IRQHandler
    def_irq_handler FLASH_IRQHandler
    def_irq_handler WKT_IRQHandler
    def_irq_handler ADC_SEQA_IRQHandler
    def_irq_handler ADC_SEQB_IRQHandler
    def_irq_handler ADC_THCMP_IRQHandler
    def_irq_handler ADC_OVR_IRQHandler
    def_irq_handler DMA_IRQHandler
    def_irq_handler I2C2_IRQHandler
    def_irq_handler I2C3_IRQHandler
    def_irq_handler CTIMER0_IRQHandler
    def_irq_handler PIN_INT0_IRQHandler
    def_irq_handler PIN_INT1_IRQHandler
    def_irq_handler PIN_INT2_IRQHandler
    def_irq_handler PIN_INT3_IRQHandler
    def_irq_handler PIN_INT4_IRQHandler
    def_irq_handler PIN_INT5_IRQHandler
    def_irq_handler PIN_INT6_IRQHandler
    def_irq_handler PIN_INT7_IRQHandler
