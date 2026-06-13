// SPDX-License-Identifier: BSD-3-Clause
//
// Global operator new/delete for LPC8xx bare-metal.
//
// The nano.specs link line (-lgcc -lc_nano -lm -lnosys) does NOT pull in the
// C++ runtime library (libstdc++_nano / libsupc++), so the compiler-generated
// calls to `operator new[]` (_Znaj) etc. are otherwise undefined at link time.
// Linking libstdc++_nano would also drag in the exception-throwing operator
// new and its unwinder, which is dead weight on a 64KB Cortex-M0+ built with
// -fno-exceptions.
//
// Instead we provide our own thin, non-throwing operators backed by newlib's
// malloc/free (heap grown by nosys _sbrk from the linker `end` symbol). On
// out-of-memory they return nullptr rather than throwing — correct behavior
// for an -fno-exceptions build.
#include <stddef.h>
#include <stdlib.h>

void* operator new(size_t size) {
    return malloc(size ? size : 1);
}

void* operator new[](size_t size) {
    return malloc(size ? size : 1);
}

void operator delete(void* ptr) {
    free(ptr);
}

void operator delete[](void* ptr) {
    free(ptr);
}

// C++14 sized-deallocation forms (the compiler may emit these instead of the
// unsized ones); size is unused since free() tracks the allocation size.
void operator delete(void* ptr, size_t) {
    free(ptr);
}

void operator delete[](void* ptr, size_t) {
    free(ptr);
}
