# fbuild VSCode Extension

Build, deploy, and monitor Arduino/ESP32 projects using [fbuild](https://pypi.org/project/fbuild/) — directly from VSCode.

## Prerequisites

Install **fbuild** via pip:

```bash
pip install fbuild
```

fbuild must be available on your `PATH`, or configure a custom Python path in the extension settings.

## Features

### Sidebar Panel

The extension adds a dedicated **fbuild** icon in the Activity Bar (left sidebar). Click it to open the configuration panel:

| Tree Item | Description |
|-----------|-------------|
| **Profile** | `Quick` · `Release` · `Debug` — click to change |
| **Action** | `Build + Deploy + Monitor` · `Build Only` · `Deploy Only` — click to change |
| **Environment** | Auto-detected from `platformio.ini` — click to pick or enter manually |
| **Monitor** | Toggle serial monitor attachment on deploy — click to toggle |
| **Go!** | Execute the selected action |

### Status Bar

A compact summary in the status bar shows your current configuration at a glance (e.g., `⚡ Release | B+D+M | auto`). Click it to open the sidebar. The **Go!** button executes instantly.

### Keyboard Shortcut

Press **Ctrl+Shift+G** (macOS: **Cmd+Shift+G**) to execute the current action without touching the mouse.

### Environment Auto-Detection

When you click "Environment" in the sidebar, the extension parses your `platformio.ini` and shows all `[env:name]` sections as quick-pick items. No more typing environment names by hand.

### Command Palette

All controls are also accessible via the Command Palette (`Ctrl+Shift+P` → "fbuild:").

## How It Works

This extension is a thin UI layer on top of the `fbuild` CLI. When you press **Go!**, it runs the appropriate `fbuild` commands in a VSCode terminal:

- **Build Only** → `fbuild build <project> -e <env> --<profile>`
- **Deploy Only** → `fbuild deploy <project> -e <env> [--monitor]`
- **Build + Deploy + Monitor** → `fbuild build ... && fbuild deploy ... --monitor`

## Extension Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `fbuild.buildProfile` | `release` | Build profile: `quick`, `release`, or `debug` |
| `fbuild.action` | `build+deploy+monitor` | Action to perform on Go! |
| `fbuild.attachMonitor` | `true` | Attach serial monitor after deploy |
| `fbuild.environment` | `""` (auto) | Target environment from `platformio.ini` |
| `fbuild.pythonPath` | `""` (system) | Path to Python with fbuild installed |

## Versioning

This extension is versioned **independently** from the fbuild CLI tool. The extension is a UI wrapper and does not bundle fbuild — install/upgrade fbuild separately via pip.

## Development

```bash
cd vscode-fbuild
npm install
npm run compile   # TypeScript → JavaScript
npm run watch     # recompile on save
npm run package   # produce .vsix for local install
```

To test locally, press `F5` in VSCode with this folder open to launch the Extension Development Host.
