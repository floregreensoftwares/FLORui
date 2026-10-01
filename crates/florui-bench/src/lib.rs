//! Reproducible performance baselines for the frame pipeline. A run measures
//! each workload in isolated processes (cold first operation, then warm
//! samples), keeps every raw sample, records the machine and build, and a
//! comparison separates a real change from noise. See `florui-bench --help`.

pub mod alloc;
pub mod overhead;
pub mod report;
pub mod runner;
pub mod stats;
pub mod workloads;
