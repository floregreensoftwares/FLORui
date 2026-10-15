//! Lock ownership plus activation handoff — see
//! `crate::os::windows::single_instance` and
//! `crate::os::linux::single_instance` for the real implementations. On
//! any other platform, single-instance enforcement is silently
//! unenforced: every launch reports [`InstanceRole::Primary`] and a
//! handoff always [`HandoffOutcome::Failed`]s, since there is no
//! equivalent IPC mechanism here yet.

#[cfg(target_os = "windows")]
pub(crate) use crate::os::windows::single_instance::{
    HandoffOutcome, InstanceRole, acquire, handoff, probe_capability, spawn_activation_listener,
};

#[cfg(target_os = "linux")]
pub(crate) use crate::os::linux::single_instance::{
    HandoffOutcome, InstanceRole, acquire, handoff, probe_capability, spawn_activation_listener,
};

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(crate) use stub::{
    HandoffOutcome, InstanceRole, acquire, handoff, probe_capability, spawn_activation_listener,
};

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod stub {
    use std::time::Duration;

    use crate::activation::ActivationEvent;

    /// Stands in for the real build's `MutexOwnership` guard, so call sites
    /// can bind and hold it uniformly across platforms.
    pub(crate) struct NoOwnership;

    #[allow(dead_code)] // Only a real implementation builds `Secondary`.
    pub(crate) enum InstanceRole {
        Primary(NoOwnership),
        Secondary,
    }

    #[allow(dead_code)] // Only a real implementation builds `Delivered`.
    pub(crate) enum HandoffOutcome {
        Delivered,
        Failed,
    }

    pub(crate) fn acquire(_app_identifier: &str) -> std::io::Result<InstanceRole> {
        Ok(InstanceRole::Primary(NoOwnership))
    }

    pub(crate) fn spawn_activation_listener(
        _app_identifier: &str,
        _on_event: impl Fn(ActivationEvent) + Send + 'static,
    ) -> std::io::Result<()> {
        Ok(())
    }

    pub(crate) fn handoff(
        _app_identifier: &str,
        _event: &ActivationEvent,
        _timeout: Duration,
    ) -> HandoffOutcome {
        HandoffOutcome::Failed
    }

    /// No equivalent mechanism exists on this platform yet -- see this
    /// module's own doc.
    pub(crate) fn probe_capability() -> bool {
        false
    }
}
