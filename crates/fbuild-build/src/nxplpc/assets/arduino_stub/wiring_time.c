// SPDX-License-Identifier: BSD-3-Clause
#include "Arduino.h"

#if defined(CPU_LPC845M301JBD48) || defined(CPU_LPC845M301JBD64)
#include "LPC845.h"
#elif defined(CPU_LPC804M101JDH24)
#include "LPC804.h"
#else
#error "Unsupported NXP LPC8xx CPU"
#endif

__attribute__((weak)) uint32_t SystemCoreClock = F_CPU;

__attribute__((weak)) void SystemCoreClockUpdate(void) {
    SystemCoreClock = F_CPU;
}

static volatile uint32_t g_systick_millis = 0;
static uint8_t g_systick_started = 0;

static uint32_t ticks_per_ms(void) {
    return F_CPU / 1000UL;
}

static uint32_t ticks_per_us(void) {
    return F_CPU / 1000000UL;
}

static void init_systick(void) {
    if (g_systick_started) {
        return;
    }

    const uint32_t ticks = ticks_per_ms();
    if (ticks == 0 || ticks > 0x1000000UL) {
        return;
    }

    SysTick->LOAD = ticks - 1;
    SysTick->VAL = 0;
    SysTick->CTRL = SysTick_CTRL_CLKSOURCE_Msk |
                    SysTick_CTRL_TICKINT_Msk |
                    SysTick_CTRL_ENABLE_Msk;
    g_systick_started = 1;
}

void SysTick_Handler(void) {
    ++g_systick_millis;
}

uint32_t millis(void) {
    init_systick();
    return g_systick_millis;
}

uint32_t micros(void) {
    init_systick();

    const uint32_t per_us = ticks_per_us();
    if (!g_systick_started || per_us == 0) {
        return g_systick_millis * 1000UL;
    }

    uint32_t before;
    uint32_t fraction;
    uint32_t after;
    do {
        before = g_systick_millis;
        const uint32_t period_ticks = SysTick->LOAD + 1;
        const uint32_t current_ticks = SysTick->VAL;
        const uint32_t elapsed_ticks = period_ticks - current_ticks;
        fraction = elapsed_ticks / per_us;
        if (fraction > 999UL) {
            fraction = 999UL;
        }
        after = g_systick_millis;
    } while (before != after);

    return before * 1000UL + fraction;
}

void delay(uint32_t ms) {
    const uint32_t start = millis();
    while ((millis() - start) < ms) {
    }
}

void delayMicroseconds(uint32_t us) {
    const uint32_t start = micros();
    while ((micros() - start) < us) {
    }
}

void yield(void) {}
