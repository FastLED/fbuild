# Assets

Embedded data files for fbuild-config.

## boards/

Board database (1609 boards) from PlatformIO registry, matching the Python fbuild structure.

- `boards/manifest.json` — sorted list of all board IDs
- `boards/json/{board_id}.json` — one file per board with config (name, MCU, CPU frequency, RAM, ROM, platform, etc.)

Source: `~/dev/fbuild/assets/boards/` (Python fbuild).

At compile time, `build.rs` combines all individual board JSON files into a single embedded blob.

To sync from Python fbuild:

```bash
cp -r ~/dev/fbuild/assets/boards/json/ crates/fbuild-config/assets/boards/json/
cp ~/dev/fbuild/assets/boards/manifest.json crates/fbuild-config/assets/boards/manifest.json
```
