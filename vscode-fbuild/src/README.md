# VSCode Extension Source

TypeScript source for the fbuild VSCode extension, providing build/deploy/monitor commands with status bar UI.

## Contents

- **`extension.ts`** -- Extension entry point; registers commands (`fbuild.go`, `fbuild.selectBuildProfile`, etc.) and wires up the runner and UI
- **`fbuildRunner.ts`** -- Wraps invocations of the `fbuild` CLI tool (build, deploy, monitor) via VSCode terminals
- **`ui.ts`** -- Status bar UI with quick-pick menus for build profiles (quick/release/debug) and actions (build, deploy, monitor)
