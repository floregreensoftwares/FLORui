//! Reproducible performance baselines for the frame pipeline. A run measures
//! each workload in isolated processes (cold first operation, then warm
//! samples), keeps every raw sample, records the machine and build, and a
//! comparison separates a real change from noise. See `florui-bench --help`.

pub mod alloc;
pub mod collections;
#[cfg(windows)]
pub mod composition;
pub mod edit_chain;
#[cfg(windows)]
pub mod edit_latency;
pub mod effects;
pub mod overhead;
pub mod phases;
pub mod present;
#[cfg(windows)]
pub mod present_run;
pub mod reload;
pub mod report;
pub mod runner;
pub mod stats;
pub mod workloads;
