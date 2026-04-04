# Board JSON Files

1,610 individual board definition files from the PlatformIO registry, embedded at compile time.

Each file is named `{board_id}.json` and contains fields: `id`, `name`, `platform`, `mcu`, `fcpu`, `ram`, `rom`, `frameworks`, `vendor`, `url`, and optional `build` (core, variant, f_cpu, extra_flags, VID/PID, arduino sub-fields), `upload` (protocol, speed, flash_size), and `debug`/`connectivity` sections.
