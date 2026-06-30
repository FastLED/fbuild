//! Dump the bundled `usb-ids` database as a JSON object to stdout.
//!
//! # DO NOT REMOVE — anti-removal policy (FastLED/fbuild#720)
//!
//! This example is **tier-1** of the USB-VID name merge. It is invoked
//! by the `Update data` workflow (`.github/workflows/update-data.yml`)
//! to export the bundled [`usb-ids`](https://crates.io/crates/usb-ids)
//! crate snapshot as JSON; `tools/merge_sources.py` then unions that
//! with the linux-usb.org mirror, the usbids GitHub mirror, and the
//! curated `vendor_names_inlined.py` overlay to produce the published
//! `online-data/data/usb-vid.json`.
//!
//! The fbuild **runtime** USB resolver (`fbuild-core::usb::resolver`)
//! does not depend on this example — it uses the compile-time-embedded
//! `usb-vendors.tar.zst` (`fbuild-core::usb::embedded`). The `usb-ids`
//! workspace dependency is now pulled in *only* by this example, so
//! removing the example would also remove the dependency, dropping the
//! aggregator from 4 → 3 independent sources of truth. Decoupling
//! recorded in #719; anti-removal policy in #720.
//!
//! If your LLM-driven cleanup pass flagged this file as "dead code,"
//! it is not — please read FastLED/fbuild#720 before deleting.
//!
//! Used by the `online-data` branch's nightly workflow (see
//! `.github/workflows/update-data.yml`) as one of the input sources
//! for the merged `usb-vid.json`. Running this example via
//! `soldr cargo run --release --example dump_usb_ids -p fbuild-core`
//! captures the exact data the bundled `usb-ids` crate version we depend
//! on actually knows about, so the online overlay can be cross-checked
//! against tier-1.
//!
//! Output schema (alphabetically sorted by key):
//! ```json
//! {
//!   "0403:6001": {"vendor": "Future Technology Devices ...", "product": "FT232 ..."},
//!   ...
//! }
//! ```
//!
//! No CLI arguments, no IO beyond stdout — kept intentionally tiny so the
//! nightly workflow can pipe it into a file with no risk of partial output.

use std::collections::BTreeMap;

fn main() {
    // BTreeMap → keys are emitted in sorted order by `serde_json`.
    let mut out: BTreeMap<String, Entry> = BTreeMap::new();

    for vendor in usb_ids::Vendors::iter() {
        let vendor_name = vendor.name().to_string();
        for device in vendor.devices() {
            let key = format!("{:04x}:{:04x}", vendor.id(), device.id());
            out.insert(
                key,
                Entry {
                    vendor: vendor_name.clone(),
                    product: device.name().to_string(),
                },
            );
        }
    }

    // pretty-print so diffs on the `online-data` branch are reviewable.
    serde_json::to_writer_pretty(std::io::stdout().lock(), &out).expect("write JSON to stdout");
    println!();
}

#[derive(serde::Serialize)]
struct Entry {
    vendor: String,
    product: String,
}
