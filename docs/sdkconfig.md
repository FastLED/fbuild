# ESP `sdkconfig` — user-override design

> **Status**: design proposal. Not yet implemented. See [ROADMAP.md](ROADMAP.md) for sequencing once accepted.

This doc covers how a fbuild user should be able to override ESP-IDF
`CONFIG_*` settings for an ESP32-family build, what surfaces exist
today, and what the resolved precedence chain should look like.

## TL;DR

Five sources funnel into one resolved `CONFIG_*` dict; highest layer
wins per key:

| # | Source | Format | Notes |
|---|---|---|---|
| 1 | Framework pre-baked `sdkconfig.h` | C `#define`s | Baseline. Never edit. |
| 2 | `fbuild.toml` `[sdkconfig]` table (*future*) | TOML `key = val` | Project-level in the eventual fbuild-native config |
| 3 | `sdkconfig.fragment` in project root | Kconfig syntax `CONFIG_FOO=y` | Recommended surface — `.ini`-free |
| 4 | `platformio.ini` `board_build.esp-idf.sdkconfig_options` | Multi-line Kconfig | PlatformIO-compat sdkconfig path |
| 5 | `platformio.ini` `build_flags = -D CONFIG_*` | Compiler flag | Quick one-off / PIO drop-in compat |

The merged dict is emitted **as `-D CONFIG_*` on the compiler line**
(correctness) AND **as a synthesized `.fbuild/build/<env>/sdkconfig.h`**
(visibility — not on the include path, just for grep / IDE / `compile_commands.json`).

Recommended primary surface for users: **`sdkconfig.fragment`** at the
project root. The other layers exist for PlatformIO compatibility and
for the future fbuild-native config file.

## What's there today

fbuild currently reads only **4 boolean keys** from a project's
`sdkconfig` file (`crates/fbuild-config/src/sdkconfig.rs:44-114`), used
solely for the `.eh_frame` strip policy:

- `CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE`
- `CONFIG_ESP_SYSTEM_PANIC_GDBSTUB`
- `CONFIG_ESP_DEBUG_OCDAWARE`
- `CONFIG_COMPILER_OPTIMIZATION_DEBUG`

It does **not** generate `sdkconfig.h` — the framework package ships
a pre-built one at
`tools/sdk/<mcu>/<flash_variant>/include/sdkconfig.h`, and that's what
every TU sees (discovered at
`crates/fbuild-packages/src/library/esp32_framework/sdk_paths.rs:54-75`).
The closest thing to a "user override" hook today is
`build_flags = -D CONFIG_FOO=1` in `platformio.ini`, which is folded
into the gcc command line in
`crates/fbuild-build/src/esp32/orchestrator/build.rs:99-106`. No
fragment file, no header overlay, no menuconfig.

## What "obsessive control matching the ESP SDK" actually means

ESP-IDF gives the user three lever points, in increasing fidelity:

1. **`sdkconfig` text** — Kconfig-formatted `CONFIG_FOO=y` / `=42` /
   `="str"` lines. The source of truth in real IDF projects.
2. **`sdkconfig.h`** — generated C header `#define CONFIG_FOO 1` from
   the above. What TUs actually `#include` (transitively).
3. **`-D CONFIG_FOO=...`** — command-line override, last-write-wins
   over the header because gcc applies `-D`s after the `#include`s.

PlatformIO collapses these into two .ini knobs:

- `build_flags = -D CONFIG_FOO=1` — direct command-line override
  (level 3 above).
- `board_build.esp-idf.sdkconfig_options = CONFIG_FOO=y` — proper
  IDF flow, but PIO uses it only when invoking actual ESP-IDF, not
  the Arduino-on-IDF path.

PlatformIO does **not** regenerate `sdkconfig.h` for Arduino-on-IDF
builds — it relies on the framework's pre-baked header. fbuild
inherits that.

## Design — 5 layers, one resolved dict, two output forms

### Precedence chain (lowest → highest)

Same table as the TL;DR; expanded rationale below.

**Layer 1: framework pre-baked `sdkconfig.h`.** Baseline. fbuild
already locates this. No change.

**Layer 2: `fbuild.toml` `[sdkconfig]` table (future).** The eventual
fbuild-native config file will host project-level configuration; the
`[sdkconfig]` table will be a 1-to-1 transcription of Kconfig
key/value pairs. Placed at layer 2 (low) because the .ini and fragment
paths stay valid forever — fbuild.toml is additive, not displacing.

**Layer 3: `sdkconfig.fragment` in project root.** The recommended
primary surface. Kconfig syntax means copy/paste from any IDF README
"just works". Lives next to `platformio.ini` / `src/`, so it's
visible without scrolling .ini sections.

**Layer 4: `platformio.ini` `board_build.esp-idf.sdkconfig_options`.**
The PlatformIO-canonical sdkconfig surface. Same Kconfig syntax as
layer 3. Higher precedence than the fragment because users who set
both probably mean the .ini to override — a common pattern is "shared
fragment in repo, per-environment overrides in .ini".

**Layer 5: `platformio.ini` `build_flags = -D CONFIG_*`.** Highest
precedence because `-D` flags are applied by gcc after the header
include. Hot-patch / one-off / drop-in PIO compat.

### Output forms

After merge, the resolved dict is **emitted twice**:

- **As `-D CONFIG_*` on the compiler command line.** This is the only
  thing actually needed for correctness when the framework
  `sdkconfig.h` is the baseline header — gcc's later `-D` wins over
  the header's earlier `#define`. This is exactly what `build_flags`
  already does today; we just expand the source set.
- **As a synthesized `.fbuild/build/<env>/sdkconfig.h`** (debug
  artifact). Not on the include path — written purely so the user
  (and downstream IDE / LSP / `compile_commands.json` consumers) can
  grep "what's actually effective for this build". Cheap to write,
  expensive *not* to have when something feels wrong.

### Why no user-owned `sdkconfig.h` in the project tree

Tempting (single file, type-checked by gcc) but two problems:

- The framework's `sdkconfig.h` is **hundreds of keys** that the user
  would have to copy or risk leaving undefined. Partial headers
  silently break things.
- Multiple includes with conflicting `#define`s → undefined-behavior
  territory. The framework header expects certain keys to be set in
  certain ways; missing one is a hardware-level footgun.

**Exception**: a user-owned `sdkconfig_extra.h` that's `-include`d
*after* the framework `sdkconfig.h` is fine for advanced users who
want `#if defined(CONFIG_X)` logic that overlay defines can't
express. Treat it as opt-in: present in project root → `-include` it.
Absent → skip. No magic.

### Why `sdkconfig.fragment` (layer 3) is the right primary surface

The stated goal — "fine levels of obsessive control that ultimately
matches the ESP SDK" — points at Kconfig syntax, not -D flags:

- **It's the format IDF and PlatformIO already use** — copy-paste from
  any IDF project README works unchanged.
- **`platformio.ini` is the wrong place** for 50-line config dumps —
  it pollutes a file that should describe the *environment*, not the
  *kernel*.
- **It's the obvious anchor for the future `fbuild.toml` migration**
  — the toml table is a 1-to-1 transcription.

Position it as the recommended way; treat `build_flags = -D CONFIG_*`
as the PIO-compat escape hatch.

### Forward-compat sketch for `fbuild.toml`

```toml
# fbuild.toml — future, NOT shipped now
[env.esp32s3]
sdkconfig = "sdkconfig.fragment"       # path; or inline below

[env.esp32s3.sdkconfig_inline]
CONFIG_ESP_SYSTEM_PANIC_PRINT_BACKTRACE = true
CONFIG_FREERTOS_UNICORE = false
CONFIG_LOG_DEFAULT_LEVEL = 3
CONFIG_MBEDTLS_CERTIFICATE_BUNDLE_DEFAULT_FULL = true
```

This sits at precedence layer 2 because `fbuild.toml` will eventually
be the source of truth — but it's never *required*; the .ini and
fragment paths stay valid forever.

## Concrete API shape

In `crates/fbuild-config/src/sdkconfig.rs` — expand from the current
4-key boolean parser:

```rust
pub enum SdkValue {
    Bool(bool),
    Int(i64),
    Hex(u64),
    Str(String),
}

pub struct SdkConfig {
    values: BTreeMap<String, SdkValue>,
    provenance: BTreeMap<String, SdkSource>,  // for "why is this set?"
}

pub enum SdkSource {
    FrameworkDefault { path: PathBuf },
    FbuildToml,
    Fragment { path: PathBuf },
    PlatformioIniSdkconfigOptions,
    PlatformioIniBuildFlags,
    CliOverride,
}

impl SdkConfig {
    pub fn from_framework_header(path: &Path) -> Result<Self>;
    pub fn from_fragment_text(text: &str) -> Result<Self>;
    pub fn from_ini(ini: &PlatformIOConfig, env: &str) -> Result<Self>;
    pub fn overlay(&mut self, higher: Self);    // higher wins per key
    pub fn to_define_flags(&self) -> Vec<String>;     // "-DCONFIG_FOO=1"
    pub fn to_synth_header(&self) -> String;           // for build dir
    pub fn get_bool(&self, key: &str) -> Option<bool>; // existing 4-key consumer
}
```

The existing `SdkConfigSummary` becomes a thin wrapper around
`SdkConfig::get_bool` for the 4 eh_frame keys — no behavioral change,
no API break for the call site at
`crates/fbuild-build/src/esp32/orchestrator/build.rs:53`.

In `crates/fbuild-build/src/esp32/orchestrator/build.rs`, the merge
happens once at build start and the resolved dict flows in two places:

1. `to_define_flags()` → appended to the existing `user_flags`
   extension at `build.rs:99-106`.
2. `to_synth_header()` → written to `<build_dir>/sdkconfig.h` purely
   as a debug artifact.

## Provenance — "where did this CONFIG come from?"

Borrow the pattern from
[`fbuild bloat`'s `referenced_by`](symbols.md#referenced_by--who-pulled-this-symbol-in)
(#459): every key in the resolved dict carries an `SdkSource` enum
tag. A `fbuild sdkconfig --explain CONFIG_FOO` (or similar) command
can answer "where did this value come from?" and what would change if
that source were removed.

## Open questions

These are the parts I'd want the next round of discussion to nail
down before implementation:

1. **Fragment filename.** `sdkconfig.fragment` matches IDF's component
   convention. `sdkconfig.local` is closer to what some IDE plugins
   look for. `sdkconfig.user` is unambiguous. Pick one and document.
2. **Per-environment fragments?** A monorepo with `[env:debug]` and
   `[env:release]` might want different fragments. Should layer 3
   support `sdkconfig.<env>.fragment` lookup, or is "use the .ini
   `sdkconfig_options` for per-env" sufficient?
3. **Diagnostics on unknown keys.** ESP-IDF's Kconfig knows the set
   of valid keys; fbuild doesn't (since it doesn't run Kconfig).
   Should we just warn ("CONFIG_FOO is not in the framework header
   — typo?") or stay silent for forward-compat with future SDK
   versions? Suggest warn-on-default, silenceable.
4. **Build cache invalidation.** Changing the fragment must
   invalidate the relevant per-TU compile cache entries. The synth
   `.fbuild/build/<env>/sdkconfig.h` artifact gives a stable
   fingerprint surface for this.

## Counter-design considered and rejected

> "Just expose `build_flags = -D CONFIG_*`. Everything else is gold
> plating."

Rejected because:

- It forces 50-line config bombs into `platformio.ini`, which is
  meant to describe the *environment*, not the kernel.
- It loses the Kconfig-syntax copy-paste path from upstream IDF
  documentation.
- It has no story for the future `fbuild.toml` migration.
- It can't express provenance (was this `-D` from user, from SDK
  default, from a deprecated PlatformIO setting?).

The 5-layer design above is strictly additive over this counter-design
— users who want the simple path still write `build_flags = -D ...`
and ignore everything else. The other layers reward users who want
more.

## Related

- Issue (future) — tracking ticket for this design.
- `crates/fbuild-config/src/sdkconfig.rs` — current 4-key boolean
  parser; the expansion point.
- `crates/fbuild-build/src/esp32/orchestrator/build.rs:53-106` —
  where the resolved dict would plug into the compile flag chain.
- `crates/fbuild-packages/src/library/esp32_framework/sdk_paths.rs:54-75`
  — framework `sdkconfig.h` discovery (layer 1).
- ESP-IDF Kconfig docs — <https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/kconfig.html>
- PlatformIO `board_build.esp-idf.sdkconfig_options` —
  <https://docs.platformio.org/en/latest/platforms/espressif32.html#configuration>
