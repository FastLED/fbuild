# `daemon_client`

HTTP client + deserialization types the CLI uses to talk to the fbuild daemon.

- `types.rs` — request/response structs that mirror the daemon's JSON schemas (`crates/fbuild-daemon/src/models.rs`). Keep field-for-field compatible so deserialization stays forgiving via `#[serde(default)]`.
- `restart_diag.rs` — evidence strings for the same-version daemon restart decision: which daemon image answered `/health`, which sibling binary the CLI compared it to, and a post-respawn check that flags a second launcher serving the port (FastLED/fbuild#1476).
- Sibling `mod.rs` (one level up at `daemon_client.rs`) — HTTP transport, daemon lifecycle, retry logic.
