// SPDX-License-Identifier: BSD-3-Clause
#pragma once

#if defined(CPU_LPC845M301JBD48)
#define LPC845_SERIES
#include "LPC845.h"
#elif defined(CPU_LPC804M101JDH24)
#define LPC804_SERIES
#include "LPC804.h"
#else
#error "No valid NXP LPC8xx CPU defined"
#endif
