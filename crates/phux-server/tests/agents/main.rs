//! Agent detection harness. Kept separate because its tests set process-wide
//! detector timing overrides; nextest still runs each test in its own process.

mod agent_detect;
