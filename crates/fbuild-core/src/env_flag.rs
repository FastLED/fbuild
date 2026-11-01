//! Canonical parsing for boolean environment flags (FastLED/fbuild#1798).
//!
//! fbuild grew three different answers to "is this `FBUILD_*` env var on?" —
//! one that accepted anything but `0`/`false`, one that accepted only `1`, and
//! one that accepted `1`/`true`/`yes` in two specific spellings. The same
//! value therefore meant different things depending on which flag you set, so
//! an operator could not carry an intuition from one flag to the next.
//!
//! This module is the single source of truth. The truth table, applied to the
//! value after trimming surrounding whitespace and case-folding, is:
//!
//! | value                                   | result |
//! |-----------------------------------------|--------|
//! | `1`, `true`, `yes`, `on`                | on     |
//! | unset, `""`, `0`, `false`, `no`, `off`  | off    |
//! | anything else                           | off    |
//!
//! Two properties matter more than the exact vocabulary:
//!
//! 1. **A malformed value is never on.** Every historical copy that defaulted
//!    unknown input to *on* turned a typo into a silent behaviour change
//!    (`=no`, `=off`, `=-1`, `=ture`). Unrecognised input is *off*, so a typo
//!    costs a feature, never its unintended activation.
//! 2. **One spelling works everywhere.** `no`/`off` are falsy and `yes`/`on`
//!    are truthy on every flag, in any case, with or without padding.
//!
//! Scope: this is the rule for *feature toggles*. A switch that relocates
//! state rather than enabling a feature keeps its own narrower contract —
//! `FBUILD_DEV_MODE` deliberately accepts only the literal `1` (FastLED/fbuild
//! #1349: a stray `FBUILD_DEV_MODE=0` used to select the dev tree), and
//! widening it is a separate decision, not a mechanical migration.

/// Accepted `true` spellings, lowercased for comparison.
const TRUE_SPELLINGS: [&str; 4] = ["1", "true", "yes", "on"];

/// Accepted `false` spellings, lowercased for comparison. The empty string is
/// included because `FOO=` exports an empty value, not an absent one.
const FALSE_SPELLINGS: [&str; 5] = ["", "0", "false", "no", "off"];

/// Parse one flag value against the canonical truth table.
///
/// `Some(true)` for a recognised truthy spelling, `Some(false)` for a
/// recognised falsy spelling (including the empty string), and `None` for a
/// value this module does not recognise. Callers almost always want
/// [`is_set`]; `parse` exists so the truth table can be exercised without
/// touching the process environment.
pub fn parse(value: &str) -> Option<bool> {
    let value = value.trim();
    if TRUE_SPELLINGS
        .iter()
        .any(|spelling| value.eq_ignore_ascii_case(spelling))
    {
        return Some(true);
    }
    if FALSE_SPELLINGS
        .iter()
        .any(|spelling| value.eq_ignore_ascii_case(spelling))
    {
        return Some(false);
    }
    None
}

/// True when the named environment variable is set to a truthy spelling.
///
/// An unset variable, an empty value, and any unrecognised value all read as
/// `false` — see the module docs for why a malformed value must never enable a
/// feature.
pub fn is_set(name: &str) -> bool {
    parse(&std::env::var(name).unwrap_or_default()) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthy_spellings_are_on() {
        for value in ["1", "true", "TRUE", "True", "yes", "YES", "on", "ON"] {
            assert_eq!(parse(value), Some(true), "{value:?} should be on");
        }
    }

    #[test]
    fn falsy_spellings_are_off() {
        for value in ["", "0", "false", "FALSE", "False", "no", "NO", "off", "OFF"] {
            assert_eq!(parse(value), Some(false), "{value:?} should be off");
        }
    }

    #[test]
    fn padding_is_ignored() {
        assert_eq!(parse(" 1 "), Some(true));
        assert_eq!(parse("\t1\n"), Some(true));
        assert_eq!(parse("  yes  "), Some(true));
        assert_eq!(parse(" 0 "), Some(false));
        assert_eq!(parse(" off "), Some(false));
        assert_eq!(parse("  go  "), None);
    }

    #[test]
    fn malformed_values_are_unrecognised_never_on() {
        // Every one of these was truthy under at least one of the three copies
        // this module replaces (`=no` and `=-1` under the fbuild-cli rule).
        for value in ["ture", "-1", "2", "y", "n", "enable", "disabled", "11"] {
            assert_eq!(parse(value), None, "{value:?} must not be recognised");
        }
    }

    #[test]
    fn is_set_reads_the_environment() {
        // A flag name nothing else in the workspace reads, so this test cannot
        // race another test's mutation of the same variable.
        const NAME: &str = "FBUILD_ENV_FLAG_CORE_TEST_1798";
        std::env::remove_var(NAME);
        assert!(!is_set(NAME), "unset must be off");

        std::env::set_var(NAME, "on");
        assert!(is_set(NAME), "=on must be on");

        std::env::set_var(NAME, "off");
        assert!(!is_set(NAME), "=off must be off");

        std::env::set_var(NAME, "  TRUE  ");
        assert!(is_set(NAME), "padded TRUE must be on");

        std::env::set_var(NAME, "ture");
        assert!(!is_set(NAME), "a typo must never turn a feature on");

        std::env::remove_var(NAME);
    }
}
