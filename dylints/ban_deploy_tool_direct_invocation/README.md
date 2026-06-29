# `ban_deploy_tool_direct_invocation`

This lint bans `Command::new("esptool" | "esptool.py" | "avrdude" |
"picotool" | "dfu-util" | "pyocd")` (and the `tokio::process::Command`
equivalent) in fbuild production code (`crates/*/src/`) outside
`crates/fbuild-deploy/`.

## Why

Per the agent docs (FastLED/fbuild#694 and `CLAUDE.md`'s "Essential
Rules"), all deploy-tool spawns must flow through `fbuild deploy`.
Direct invocation outside `fbuild-deploy` regresses the deploy contract:
no `Deployer::post_deploy_recovery`, no consistent error reporting, no
serial-port hand-off — and bypasses the `fbuild deploy --to emu`
emulator routing.

This lint complements `ban_raw_subprocess` (which catches
`.spawn()/.output()/.status()` on `Command` regardless of the binary).
`ban_raw_subprocess` is scoped to *how* you spawn; this one is scoped to
*what* you spawn. Together they form an L-shape: even with legitimate
allowlisted raw-spawn entries, you still can't spawn a deploy tool
outside `fbuild-deploy`.

## Scope

Only files whose path contains BOTH `crates/` and a subsequent `/src/`
segment are linted. Files anywhere under `crates/fbuild-deploy/` are
unconditionally exempt — that crate is the legitimate owner.

## Banned binary names

Matched against the string literal passed to `Command::new(...)` (case
sensitive):

- `esptool`
- `esptool.py`
- `avrdude`
- `picotool`
- `dfu-util`
- `pyocd`

The match is exact. Paths like `/usr/local/bin/esptool` and quoted-arg
shapes (`Command::new(path).arg("flash")`) won't trip the lint — those
are caller-resolved paths and are likely going through `fbuild-deploy`
already. The lint is about the *quick string-literal sketch* that's
easy to add and easy to miss in review.

## Allowlist

Empty. `crates/fbuild-deploy/` is the only legitimate caller and is
exempted by directory match, not by allowlist.

## See also

- FastLED/fbuild#826 — the dylint sweep tracking issue
- FastLED/fbuild#694 — the `fbuild deploy` ownership rule
- `crates/fbuild-deploy/` — the legitimate owner
- `dylints/ban_raw_subprocess/` — the spawn-shape sibling
