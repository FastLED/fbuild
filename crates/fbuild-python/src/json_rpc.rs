//! REMOTE: JSON-RPC response helpers shared by the serial-session core and
//! its facades.
//!
//! The async read/write loops that used to live here were deleted as part
//! of FastLED/fbuild#1485: both `SerialMonitor` and `AsyncSerialMonitor`
//! now sit on top of `crate::serial_session::SerialSession`, which owns its
//! own reader task and reply/RPC FIFOs.

#[cfg(test)]
pub(crate) fn extract_remote_json_rpc_response(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        line.strip_prefix("REMOTE:")
            .map(|json_part| json_part.to_string())
    })
}
