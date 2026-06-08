// SPDX-License-Identifier: BSD-3-Clause
// Minimal CMSIS device header for fbuild's LPC804 target.
#pragma once

#include <stdint.h>

#define NUMBER_OF_INT_VECTORS 48

typedef enum IRQn {
    NonMaskableInt_IRQn = -14,
    HardFault_IRQn = -13,
    SVCall_IRQn = -5,
    PendSV_IRQn = -2,
    SysTick_IRQn = -1,
    SPI0_IRQn = 0,
    USART0_IRQn = 3,
    USART1_IRQn = 4,
    I2C1_IRQn = 7,
    I2C0_IRQn = 8,
    SCT_IRQn = 9,
    MRT_IRQn = 10,
    WDT_IRQn = 12,
    BOD_IRQn = 13,
    WKT_IRQn = 15,
    ADC_SEQA_IRQn = 16,
    ADC_SEQB_IRQn = 17,
    ADC_THCMP_IRQn = 18,
    ADC_OVR_IRQn = 19,
    DMA_IRQn = 20,
    I2C2_IRQn = 21,
    I2C3_IRQn = 22,
    CTIMER0_IRQn = 23,
    PIN_INT0_IRQn = 24,
    PIN_INT1_IRQn = 25,
    PIN_INT2_IRQn = 26,
    PIN_INT3_IRQn = 27,
    PIN_INT4_IRQn = 28,
    PIN_INT5_IRQn = 29,
    PIN_INT6_IRQn = 30,
    PIN_INT7_IRQn = 31
} IRQn_Type;

#define __CM0PLUS_REV 0x0000U
#define __MPU_PRESENT 0
#define __VTOR_PRESENT 1
#define __NVIC_PRIO_BITS 2
#define __Vendor_SysTickConfig 0

#include "core_cm0plus.h"
#include "system_LPC804.h"
