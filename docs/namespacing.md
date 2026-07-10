# Env-driven artifact namespacing (`EnvNamespace`)

FastLED/fbuild#574. A PlatformIO `[env:*]` block names the three coordinates
that determine where every artifact fbuild fetches or generates for that
environment lives:

```ini
[env:lpc845brk]
platform  = nxplpc
board     = lpc845brk
framework = arduino
build_flags = -DRELEASE
```

`fbuild_core::EnvNamespace { env_id, platform, board, framework }` captures that
triplet (plus the env id) as one typed value so routing is keyed on a single
namespace instead of ad-hoc string plumbing.

## Where it comes from

`BuildContext::env_namespace(env_id, platform)`
(`fbuild-build-engine/src/pipeline/context.rs`) builds it from the parsed
`platformio.ini` for the current build: `board` and `framework` from the env
section, `platform` from the orchestrator (which already knows it), `env_id`
from `BuildParams.env_name`. Every orchestrator's `build()` already resolves all
four during `BuildContext::new`.

## Namespace layout

| Artifact | Segment | Keyed on |
|---|---|---|
| framework source tree | `framework-<framework>-<platform>/…` | `EnvNamespace::framework_segment()` |
| platform SDK / toolchain | `platform-<platform>/…` | `platform` |
| board definition + variant + linker | `board-<board>/…` | `board` |
| per-env build output | `build/<slug>/<profile>/…` | `EnvNamespace::slug()` (`<env_id>-<board>`) |
| env-scoped library cache | `lib/<library>/…` under the env slug | `slug` + flags hash |

`slug()` and `framework_segment()` sanitize to a single filesystem-safe path
segment on every OS. Two envs that share a `platform` but differ in
`board`/`env_id` get distinct slugs (so they isolate board variant + linker +
per-env build output) while still being able to share the platform/framework
cache.

## `build_flags` propagation

`[env:*] build_flags` are the canonical place to inject `-D` defines, and they
must reach **framework/core, library, AND sketch** translation units uniformly.
This is enforced structurally by a single overlay-assembly seam rather than
per-orchestrator copies:

- `BuildContext::compile_overlays() -> (user_overlay, src_overlay)` is the one
  source of truth. `user_overlay` (= `build_flags` + global script overlay) is
  applied to core/framework + library compiles; `src_overlay` (= `user_overlay`
  + `build_src_flags` + project script overlay) is applied to sketch + local-lib
  compiles. The shared sequential pipeline, the ESP32 orchestrator, and the
  nxplpc orchestrator all call it (previously each had a hand-copied version).
- Caller-injected one-off flags (`BuildParams.extra_build_flags`, e.g. QEMU
  emulation defines) are folded into `user_flags` in `BuildContext::new`, so
  they now propagate on **every** orchestrator — previously the sequential
  pipeline dropped them and only ESP32 applied them.

The pure `assemble_compile_overlays` function is unit-tested to guarantee
`build_flags` reach both overlays and `build_src_flags` reach only the sketch
overlay.

## Status / scope

This lands the typed `EnvNamespace` (success criterion 1), the uniform
`build_flags` propagation guarantee via a single de-duplicated overlay seam
(criterion 3), and this doc (criterion 4). Threading `EnvNamespace` into every
package fetcher and cache-path deriver (criterion 2's full breadth) and the
`fbuild cache gc --env <id>` reconciliation are follow-ups that build on this
foundation.
