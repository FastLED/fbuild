# Changelog

All notable changes to the fbuild VSCode extension will be documented in this file.

## [0.2.0] - 2026-03-26

### Added
- **Sidebar panel** with Activity Bar icon (lightning bolt) for organized configuration
- **Tree view** showing Profile, Action, Environment, Monitor, and Go! as clickable items
- **Environment auto-detection** — parses `platformio.ini` to show `[env:name]` sections as quick-pick items
- **Compact status bar** — single summary item (`⚡ Release | B+D+M | auto`) + Go! button replaces 5 separate items
- **Keyboard shortcut** — `Ctrl+Shift+G` / `Cmd+Shift+G` to execute without touching the mouse
- **Welcome view** — helpful guidance when no `platformio.ini` is detected

### Changed
- Status bar reduced from 5 items to 2 (compact summary + Go!) — less clutter, more space for other extensions
- Environment picker now shows detected environments from `platformio.ini` instead of requiring manual text input

## [0.1.0] - 2026-03-26

### Added
- Initial release
- Status bar controls for build profile (Quick / Release / Debug)
- Status bar controls for action (Build + Deploy + Monitor / Build Only / Deploy Only)
- Environment selector (auto-detect or manual entry)
- Monitor toggle (attach serial monitor on deploy)
- Go! button to execute selected action
- Configurable Python path for fbuild CLI
- Activates automatically when `platformio.ini` is detected in workspace
