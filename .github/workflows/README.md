# GitHub Actions Workflows

CI/CD workflows for the fbuild project, covering lint, test, documentation, and binary builds.

## CI Checks (push/PR)

- **`check-{macos,ubuntu,windows}.yml`** -- Clippy + tests per platform
- **`fmt.yml`** -- Rustfmt check | **`docs.yml`** -- Doc build with `-D warnings`
- **`msrv.yml`** -- MSRV 1.75 verification | **`validate-boards.yml`** -- Board JSON validation

## Per-Board Builds (push/PR)

- **`build-esp32{c2,c3,c6,dev,s3}.yml`** -- ESP32 variants
- **`build-esp8266.yml`**, **`build-uno.yml`** -- AVR/Xtensa boards
- **`build-teensy{36,40,41,lc}.yml`** -- Teensy variants

## Native Binaries and Templates

- **`build.yml`** -- Manual dispatch: cross-platform native binary builds
- **`template_build.yml`** -- Reusable workflow for per-board firmware builds
- **`template_native_build.yml`** -- Reusable workflow for native Rust binary builds
