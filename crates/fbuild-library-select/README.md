# fbuild-library-select

PlatformIO-LDF-style library resolver. Given a set of project seed sources, a
list of framework libraries, and the project's include roots, it returns the
set of framework libraries transitively reachable from the seeds plus the
compile-set for each selected library.

The resolver uses path-prefix attribution (PlatformIO's `search_deps_recursive`
semantics, not basename matching): each `#include` is first resolved to an
absolute path via the walker, then attributed to whichever library's
`include_dirs` contain the resolved path as a prefix. This handles Teensyduino
/ STM32duino / Arduino layouts uniformly.

Convergence is two-pass:

1. BFS from project seeds. Any library whose header is reached is marked
   dependent and its other headers are enqueued.
2. One reconciliation pass over each dependent library's full source set to
   catch anything the header-only pass missed.

This is exactly what PlatformIO LDF chain mode does, just without the Python
overhead.
