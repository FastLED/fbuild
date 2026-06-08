// SPDX-License-Identifier: BSD-3-Clause
#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DEFAULT_SYSTEM_CLOCK 30000000u

extern uint32_t SystemCoreClock;
void SystemInit(void);
void SystemCoreClockUpdate(void);

#ifdef __cplusplus
}
#endif
