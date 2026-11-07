//! Test-only fixture budget shared by the unit tests (`src/tests.rs`) and the
//! integration test (`tests/process.rs`) via `#[path]`. Not part of the binary.

/// Setup/load budget for successful fake `ft` spawns. Interpreter startup,
/// Python imports and scheduling under ordinary host load are setup, not the
/// readiness behavior under test. Production defaults (600 s) are unaffected.
pub const FIXTURE_STARTUP_SECS: u64 = 5;
