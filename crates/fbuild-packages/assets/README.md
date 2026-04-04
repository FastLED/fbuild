# Assets

Data-driven configuration files embedded at compile time via `include_str!`.

## Files

- **`avr_frameworks.json`** -- Maps AVR board core names (e.g., `"arduino"`, `"tiny"`) to Arduino framework packages (GitHub URLs, versions, validation paths). Used by `AvrFramework` to resolve the correct framework without hardcoded values.
