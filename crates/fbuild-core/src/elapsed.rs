//! Elapsed-time formatting for build output lines.
//!
//! Provides a pure function to format a `Duration` as a space-padded prefix
//! suitable for prepending to build log lines.

use std::time::Duration;

/// Format a duration as a space-padded elapsed-time prefix.
///
/// Returns a string with format `{:7.2} ` — right-aligned, 2 decimal places,
/// minimum 7 characters wide, with a trailing space separator.
///
/// Examples: `"   0.46 "`, `"  35.12 "`, `" 172.10 "`
pub fn format_elapsed(elapsed: Duration) -> String {
    format!("{:7.2} ", elapsed.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_zero() {
        assert_eq!(format_elapsed(Duration::ZERO), "   0.00 ");
    }

    #[test]
    fn format_elapsed_sub_second() {
        assert_eq!(format_elapsed(Duration::from_millis(460)), "   0.46 ");
    }

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(format_elapsed(Duration::from_millis(35120)), "  35.12 ");
    }

    #[test]
    fn format_elapsed_large() {
        assert_eq!(format_elapsed(Duration::from_millis(172100)), " 172.10 ");
    }

    #[test]
    fn format_elapsed_over_thousand() {
        assert_eq!(format_elapsed(Duration::from_secs(1000)), "1000.00 ");
    }
}
