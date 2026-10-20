#![allow(dead_code)]

use proptest::test_runner::Config;

pub mod distributions;
pub mod run_config;
pub mod transforms;

/// Distribution and transform suites use 64 cases, compact configuration
/// suites use 32, and schema validation uses 24. Bounded scalars and at most
/// three assets/cameras keep the complete focused command below a few seconds
/// on a developer laptop.
pub fn suite_config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}
