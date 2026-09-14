# Cross-compilation

How fbuild's release binaries and wheels get built for platforms the
runner is not. **Read this before touching
[`.github/workflows/template_native_build.yml`](../../.github/workflows/template_native_build.yml)
or any release lane.**

## The blessed path

soldr owns cross-compilation. Two commands, identical shape for every
supported triple:

```bash
soldr prepare --target <triple>            # stdlib + compiler/linker + SDK/sysroot + env
soldr build --release --target <triple> -p <crate>
```

`soldr prepare --help` states the contract: it "installs the Rust
standard library, selects and materializes the blessed compiler/linker
plus SDK or sysroot, and exports the target-scoped environment. Legacy
backend wrappers are diagnostic-only overrides and are never selected by
this command."

**`cargo-zigbuild`, `ziglang`, `zig cc`/`zig c++`, and `cargo-xwin` are
banned**, enforced by `ci/check_no_legacy_cross.py` (unit tests in
`ci/test_no_legacy_cross.py`). The gate scans runnable lines only —
comments may name them, which is how the history below stays readable.

soldr's supported targets, as reported by its own error text when asked
for something else:

```
x86_64-pc-windows-msvc, x86_64-pc-windows-gnu, aarch64-pc-windows-msvc,
x86_64-apple-darwin, aarch64-apple-darwin,
x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu,
x86_64-unknown-linux-musl, aarch64-unknown-linux-musl
```

## The trap: never hand soldr a zigbuild target suffix

**This is the mistake not to repeat.** It cost hours and nearly cost a
lane of the migration.

manylinux wheels are glibc-based: a `manylinux_2_17` wheel promises it
runs on glibc >= 2.17. Nothing in the build or the packaging checks that
promise -- `ci/publish.py` assigns the tag from a filename map, not by
measuring the binary. So a `.so` linked against a newer glibc gets tagged
`manylinux_2_17`, uploads fine, installs fine, and fails at **import**
on any distro older than the build host.

**soldr holds the floor by itself.** Its catalogue sysroot for
`x86_64-unknown-linux-gnu` produces an extension topping out at
**GLIBC_2.16** -- below the 2.17 floor, and below what the retired
`cargo zigbuild --target ...-gnu.2.17` lane produced:

| Built with | Max GLIBC symbol |
|---|---|
| `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` (retired) | 2.17 |
| `soldr build --target x86_64-unknown-linux-gnu` | **2.16** |
| `soldr build --target x86_64-unknown-linux-gnu.2.17` | **2.39** |

Read that third row again. **Do not port zigbuild's `.<major>.<minor>`
suffix onto a soldr target.** soldr has no such target. It logs the miss
as a *warning*, falls back to the bare host toolchain, and still exits 0:

```
soldr build: catalogue zstd sysroot unavailable for x86_64-unknown-linux-gnu.2.17:
  unsupported platform: no zstd sysroot recipe for target x86_64-unknown-linux-gnu.2.17;
  supported: [... "x86_64-unknown-linux-gnu" ...]
error: error loading target specification:
  could not find specification for target "x86_64-unknown-linux-gnu.2.17"
... exit code 0
```

The output lands in `target/.../x86_64-unknown-linux-gnu/release/` with
the suffix normalized away, so the path looks right too. The only signal
that anything went wrong is the glibc floor of the artifact.

Carrying a habit from the old backend into the new one produced a
*worse* result than either doing nothing or doing it right. When
migrating a toolchain, re-derive the invocation from the new tool's own
docs; do not translate the old flags.

## Always verify the artifact, never the exit code

Both failure modes in this doc produced **exit 0**. Check the binary:

```bash
# glibc floor of a Linux .so — anything above 2.17 is a broken wheel
objdump -T <lib>.so | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -4

# Mach-O arch (no `file` on some hosts): cffaedfe = Mach-O 64 LE,
# cputype 0x100000c = arm64, 0x1000007 = x86_64
python3 -c "d=open('<bin>','rb').read(8); print(d[:4].hex(), hex(int.from_bytes(d[4:8],'little')))"
```

The published wheel is downloadable, so the shipped floor is always
checkable after the fact:

```bash
curl -sL <wheel-url> -o w.whl && python3 -c "import zipfile;zipfile.ZipFile('w.whl').extractall('x')"
objdump -T x/fbuild/_native.abi3.so | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail
```

## Pin every toolchain that can float

On 2026-09-03 the 2.5.22 release failed both apple-darwin lanes:

```
error: unable to read exported symbols list '-dead_strip': FileNotFound
error: could not compile `zccache-watcher` (lib) due to 1 previous error
```

Cause: the template ran `pip install cargo-zigbuild` **unpinned**. The
last good release built on 0.23.1; that day pip served 0.23.4, which
reorders the `-Wl,-exported_symbols_list` / `-Wl,<path>` pair rustc
emits for a cdylib so zig reads the following flag as the list path.
Nothing in fbuild had changed.

The rule: **a release lane may not install a floating version of
anything.** soldr's binary version is pinned explicitly in the workflow;
any remaining pip install is pinned too. When a pin moves, prove it with
a release build before merging.

## Why the version pin matters more than it looks

`zackees/setup-soldr@v0` is a floating major tag; the `version:` input
pins only the soldr *binary*, not the action. If a cross lane breaks
with no corresponding fbuild change, compare the action SHA and the
installed tool versions against the last good run before touching
fbuild's own code:

```bash
gh run view --job <id> --repo FastLED/fbuild --log | grep -E "Download action repository|Successfully installed"
```

That diff is what identified the cargo-zigbuild regression above in
minutes.
