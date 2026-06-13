/*
 * Startup / vector table for NXP LPC804 (Cortex-M0+).
 *
 * Provides:
 *   - Initial stack pointer
 *   - Reset_Handler (copies .data, zeros .bss, calls SystemInit then main)
 *   - Expanded vector table covering ARMv6-M system vectors + LPC804
 *     peripheral IRQs (see UM11065 table 4 "Interrupt sources")
 *   - SystemInit that explicitly leaves the FRO at 15 MHz IRC default
 *     and programs 0 flash wait states (matches `F_CPU=15000000UL` in
 *     FastLED `led_sysdefs_arm_lpc.h`)
 *
 * All non-Reset vectors alias Default_Handler (weak). Drivers may override
 * any IRQ by defining the corresponding `<Name>_IRQHandler` symbol.
 *
 * Design note (FastLED/FastLED#2845):
 *   The LPC804 PLL is *deliberately not configured* here. Reasons:
 *     1. The 15 MHz FRO default matches the `F_CPU=15000000UL` advertised in
 *        FastLED's `led_sysdefs_arm_lpc.h`. Raising it would silently break
 *        bit-bang cycle counts on the Stage 2a driver.
 *     2. The Stage 2b PLU clockless driver (FastLED/FastLED#2841) may want
 *        a separate timing-counter clock that the PLU configures itself.
 *        SystemInit should not pre-commit a PLL setup that the driver then
 *        has to tear down.
 *   If a future driver needs PLL-based clocking it should provide a strong
 *   override of `SystemInit`.
 *
 * References:
 *   - UM11065 (LPC80x User Manual), section 4 (system config), section 4.5
 *     (FMC FLASHCFG), table 4 (interrupt sources)
 *     https://www.nxp.com/docs/en/nxp/data-sheets/LPC804_DS.pdf
 *   - NXP CMSIS device header `LPC804.h` (register layout)
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
    /* Boot-ROM vector checksum, computed by the linker script (see lpc804.ld). */
    .extern _isr_vector_checksum

/* ---------------------------------------------------------------------------
 * Vector table — placed in flash at 0x00000000 via .isr_vector.
 *
 * Layout: 16 ARMv6-M system entries + LPC804 peripheral IRQs
 * (UM11065 table 4). Vectors 16..47 correspond to IRQ#0..IRQ#31 in the
 * NVIC view. LPC804 has fewer peripherals than LPC845 (no second DMA,
 * fewer pin-interrupts, no CTIMER); reserved slots are zero-filled.
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

    /* LPC804 peripheral IRQs (UM11065 table 4).
     * IRQ numbers per CMSIS device header `LPC804.h`. */
    .word SPI0_IRQHandler        /* IRQ  0 — SPI0 */
    .word 0                      /* IRQ  1 — reserved (no SPI1 on LPC804) */
    .word DAC0_IRQHandler        /* IRQ  2 — DAC0 */
    .word USART0_IRQHandler      /* IRQ  3 — USART0 */
    .word USART1_IRQHandler      /* IRQ  4 — USART1 */
    .word USART2_IRQHandler      /* IRQ  5 — USART2 */
    .word 0                      /* IRQ  6 — reserved */
    .word I2C1_IRQHandler        /* IRQ  7 — I2C1 */
    .word I2C0_IRQHandler        /* IRQ  8 — I2C0 */
    .word SCT_IRQHandler         /* IRQ  9 — SCT (LPC804 also has SCT) */
    .word MRT_IRQHandler         /* IRQ 10 — Multi-rate timer */
    .word CMP_CAPT_IRQHandler    /* IRQ 11 — Analog comparator / CAPT */
    .word WDT_IRQHandler         /* IRQ 12 — Windowed watchdog */
    .word BOD_IRQHandler         /* IRQ 13 — Brown-out detect */
    .word FLASH_IRQHandler       /* IRQ 14 — Flash controller */
    .word WKT_IRQHandler         /* IRQ 15 — Wake-up timer */
    .word ADC_SEQA_IRQHandler    /* IRQ 16 — ADC sequence A complete */
    .word ADC_SEQB_IRQHandler    /* IRQ 17 — ADC sequence B complete */
    .word ADC_THCMP_IRQHandler   /* IRQ 18 — ADC threshold compare */
    .word ADC_OVR_IRQHandler     /* IRQ 19 — ADC overrun */
    .word DMA_IRQHandler         /* IRQ 20 — DMA0 (LPC804 has only one DMA) */
    .word I2C2_IRQHandler        /* IRQ 21 — I2C2 */
    .word I2C3_IRQHandler        /* IRQ 22 — I2C3 */
    .word PLU_IRQHandler         /* IRQ 23 — PLU (LPC804 distinguishing feature) */
    .word PIN_INT0_IRQHandler    /* IRQ 24 — Pin interrupt 0 */
    .word PIN_INT1_IRQHandler    /* IRQ 25 — Pin interrupt 1 */
    .word PIN_INT2_IRQHandler    /* IRQ 26 — Pin interrupt 2 */
    .word PIN_INT3_IRQHandler    /* IRQ 27 — Pin interrupt 3 */
    .word 0                      /* IRQ 28 — reserved (no PINT4 on LPC804) */
    .word 0                      /* IRQ 29 — reserved */
    .word 0                      /* IRQ 30 — reserved */
    .word 0                      /* IRQ 31 — reserved */

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
    /* SystemInit explicitly leaves FRO at 15 MHz default. */
    bl      SystemInit
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
 * SystemInit — minimal clock setup for LPC804 @ 15 MHz IRC default.
 *
 * UM11065 section 4 boot defaults:
 *   - Core clock source: FRO @ 15 MHz (post-divide-by-2 from 30 MHz osc)
 *   - SYSAHBCLKDIV = 1
 *   - PLL: off
 *
 * What this routine does:
 *   1. Program flash wait states: 0 wait states is valid up to ~20 MHz on
 *      LPC804 flash (UM11065 §4.5 FLASHCFG.FLASHTIM). At 15 MHz this is safe.
 *   2. Set SYSAHBCLKDIV = 1 explicitly (matches reset default).
 *   3. Leave SYSCON->MAINCLKSEL untouched (FRO direct = reset default).
 *
 * What this routine does *not* do:
 *   - PLL configuration. The Stage 2b PLU clockless driver
 *     (FastLED/FastLED#2841) may want to own clocking decisions for the
 *     timing-counter path; bare SystemInit should not pre-commit.
 *
 * Register addresses (UM11065, CMSIS LPC804.h):
 *   SYSCON base   = 0x40048000
 *     SYSAHBCLKDIV @ 0x078
 *   FMC base      = 0x40040000
 *     FLASHCFG     @ 0x010
 * --------------------------------------------------------------------------- */
    .equ LPC804_FMC_BASE,         0x40040000
    .equ LPC804_FMC_FLASHCFG_OFS, 0x010
    .equ LPC804_SYSCON_BASE,      0x40048000
    .equ LPC804_SYSCON_SYSAHBCLKDIV_OFS, 0x078

    /* FLASHTIM[1:0] = 0 → 0 wait states (valid up to 20 MHz). */
    .equ LPC804_FLASHCFG_0WS,     0x0
    .equ LPC804_FLASHCFG_TIM_MASK, 0x3

    .weak SystemInit
    .thumb_func
    .type SystemInit, %function
SystemInit:
    /* (1) FLASHCFG = (FLASHCFG & ~0x3) | 0   — set 0 wait states. */
    ldr     r0, =LPC804_FMC_BASE
    ldr     r1, [r0, #LPC804_FMC_FLASHCFG_OFS]
    movs    r2, #LPC804_FLASHCFG_TIM_MASK
    bics    r1, r2
    /* No OR needed: FLASHTIM = 0. */
    str     r1, [r0, #LPC804_FMC_FLASHCFG_OFS]

    /* (2) Ensure SYSAHBCLKDIV = 1. */
    ldr     r0, =LPC804_SYSCON_BASE
    movs    r1, #1
    str     r1, [r0, #LPC804_SYSCON_SYSAHBCLKDIV_OFS]

    /* Reset default FRO @ 15 MHz IRC remains in effect. */
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

    /* Peripheral IRQs (UM11065 table 4). */
    def_irq_handler SPI0_IRQHandler
    def_irq_handler DAC0_IRQHandler
    def_irq_handler USART0_IRQHandler
    def_irq_handler USART1_IRQHandler
    def_irq_handler USART2_IRQHandler
    def_irq_handler I2C1_IRQHandler
    def_irq_handler I2C0_IRQHandler
    def_irq_handler SCT_IRQHandler
    def_irq_handler MRT_IRQHandler
    def_irq_handler CMP_CAPT_IRQHandler
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
    def_irq_handler PLU_IRQHandler
    def_irq_handler PIN_INT0_IRQHandler
    def_irq_handler PIN_INT1_IRQHandler
    def_irq_handler PIN_INT2_IRQHandler
    def_irq_handler PIN_INT3_IRQHandler
