# `fbuild-core` examples

Standalone runnable examples exercising public `fbuild_core` APIs.

- `dump_usb_ids.rs` — dumps the bundled `usb-ids` database as a sorted JSON object to stdout. Consumed by the `online-data` branch's nightly workflow as one input source for the merged `usb-vid.json`. Run with: `soldr cargo run --release --example dump_usb_ids -p fbuild-core > usb-ids-rs.json`.
