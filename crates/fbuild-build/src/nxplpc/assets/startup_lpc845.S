/*
 * Minimal startup / vector table for NXP LPC845 (Cortex-M0+).
 *
 * Provides: Reset_Handler (copies .data, zeros .bss, calls SystemInit then main).
 * All other vectors alias Default_Handler (weak; users may override).
 *
 * TODO(stage2): expand the IRQ table to cover the full LPC845 peripheral
 * vector list (USART0..4, I2C0..3, SPI0/1, SCT, ADC, DMA, etc.) once a
 * driver needs an actual handler. The 16-entry skeleton below is the
 * ARMv6-M system-vector minimum; bare-metal code that never enables an
 * IRQ runs fine without the extras.
 *
 * TODO(stage2): LPC845 boots at 12 MHz IRC by default; SystemInit() is a
 * placeholder. Stage 2 should configure FRO + PLL to reach the 30 MHz
 * `f_cpu` advertised in the board JSON, and configure flash wait states
 * via the FMC register block (see LPC84x UM section 4 — Flash and EEPROM).
 *
 * Reference: LPC84x user manual UM11029, table 5 ("Interrupt sources").
 *   https://www.nxp.com/docs/en/data-sheet/LPC84x.pdf
 */

    .syntax unified
    .cpu cortex-m0plus
    .thumb

    .global g_pfnVectors
    .global Default_Handler
    .global Reset_Handler

    .extern _estack
    .extern _sidata
    .extern _sdata
    .extern _edata
    .extern _sbss
    .extern _ebss
    .extern main

/* Vector table (placed in flash at 0x00000000 via .isr_vector). */
    .section .isr_vector, "a", %progbits
    .type g_pfnVectors, %object
    .size g_pfnVectors, . - g_pfnVectors

g_pfnVectors:
    .word _estack              /* 0x00 Initial stack pointer */
    .word Reset_Handler        /* 0x04 Reset */
    .word NMI_Handler          /* 0x08 NMI */
    .word HardFault_Handler    /* 0x0C HardFault */
    .word 0                    /* 0x10 reserved */
    .word 0                    /* 0x14 reserved */
    .word 0                    /* 0x18 reserved */
    .word 0                    /* 0x1C reserved (was sign for App CRC) */
    .word 0                    /* 0x20 reserved */
    .word 0                    /* 0x24 reserved */
    .word 0                    /* 0x28 reserved */
    .word SVC_Handler          /* 0x2C SVCall */
    .word 0                    /* 0x30 reserved */
    .word 0                    /* 0x34 reserved */
    .word PendSV_Handler       /* 0x38 PendSV */
    .word SysTick_Handler      /* 0x3C SysTick */

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
    /* Call SystemInit (weak — Stage 2 should override for 30 MHz PLL). */
    bl      SystemInit
    bl      main
LoopForever:
    b       LoopForever
    .size Reset_Handler, . - Reset_Handler

    .thumb_func
    .type Default_Handler, %function
Default_Handler:
    b       Default_Handler
    .size Default_Handler, . - Default_Handler

/* Weak default SystemInit — boots at the 12 MHz IRC reset default. */
    .weak SystemInit
    .thumb_func
    .type SystemInit, %function
SystemInit:
    bx      lr
    .size SystemInit, . - SystemInit

/* Weak aliases for all system handlers. */
    .macro def_irq_handler handler_name
    .weak \handler_name
    .thumb_set \handler_name, Default_Handler
    .endm

    def_irq_handler NMI_Handler
    def_irq_handler HardFault_Handler
    def_irq_handler SVC_Handler
    def_irq_handler PendSV_Handler
    def_irq_handler SysTick_Handler
