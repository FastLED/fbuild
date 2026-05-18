//! Unit tests for the `MonitorOutcome` -> `EmulatorOutcome` mapping.

use super::shared::monitor_outcome_to_emulator;
use crate::handlers::operations::MonitorOutcome;
use fbuild_core::emulator::EmulatorOutcome;

#[test]
fn monitor_outcome_to_emulator_maps_success() {
    let outcome = monitor_outcome_to_emulator(MonitorOutcome::Success("ok".into()), Some(0));
    assert_eq!(outcome, EmulatorOutcome::Passed("ok".into()));
}

#[test]
fn monitor_outcome_to_emulator_maps_error() {
    let outcome = monitor_outcome_to_emulator(MonitorOutcome::Error("bad".into()), Some(1));
    assert_eq!(outcome, EmulatorOutcome::Failed("bad".into()));
}

#[test]
fn monitor_outcome_to_emulator_maps_crash() {
    let outcome = monitor_outcome_to_emulator(
        MonitorOutcome::Error("abort() was called at PC 0x4200".into()),
        Some(134),
    );
    assert_eq!(
        outcome,
        EmulatorOutcome::Crashed("abort() was called at PC 0x4200".into())
    );
}

#[test]
fn monitor_outcome_to_emulator_maps_timeout() {
    let outcome =
        monitor_outcome_to_emulator(MonitorOutcome::Timeout { expect_found: true }, None);
    assert_eq!(outcome, EmulatorOutcome::TimedOut { expect_found: true });
}
