# fbuild VSCode Extension

Build, deploy, and monitor Arduino/ESP32 projects using [fbuild](https://pypi.org/project/fbuild/) — directly from VSCode.

## Prerequisites

Install **fbuild** via pip:

```bash
pip install fbuild
```

fbuild must be available on your `PATH`, or configure a custom Python path in the extension settings.

## Features

The extension adds status-bar controls for one-click embedded development:

| Control | Description |
|---------|-------------|
| **Build Profile** | `Quick` · `Release` · `Debug` |
| **Action** | `Build + Deploy + Monitor` · `Build Only` · `Deploy Only` |
| **Environment** | Target environment from `platformio.ini` (auto-detect or manual) |
| **Monitor** | Toggle serial monitor attachment on deploy |
| **Go!** | Execute the selected action |

All controls are accessible from the status bar and via the Command Palette (`Ctrl+Shift+P` → "fbuild:").

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
