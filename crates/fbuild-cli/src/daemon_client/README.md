# `daemon_client`

HTTP client + deserialization types the CLI uses to talk to the fbuild daemon.

- `types.rs` — request/response structs that mirror the daemon's JSON schemas (`crates/fbuild-daemon/src/models.rs`). Keep field-for-field compatible so deserialization stays forgiving via `#[serde(default)]`.
- Sibling `mod.rs` (one level up at `daemon_client.rs`) — HTTP transport, daemon lifecycle, retry logic.
