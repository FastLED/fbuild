# WebSocket handler modules

`monitor_session.rs` implements the named `/ws/monitor/:session_id` endpoint.
It is separate from the serial-monitor handler to keep each Rust source below
the repository's 1,000-line CI limit.
