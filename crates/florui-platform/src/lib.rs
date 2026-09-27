//! [`UiRuntime`]: the window-independent half of a running Florui tree —
//! rendering, hit testing, and click dispatch, with no dependency on any
//! particular window system. The `desktop` feature (on by default) adds
//! [`run`], a real `winit`/`softbuffer` desktop host built on top of it,
//! so a caller doesn't have to write its own desktop event loop just to
//! see a component tree running, and [`run_with_css_reload`], the same
//! host watching its stylesheet on disk so an edit reaches the window
//! without resetting any component state; a mobile or game host that
//! wants [`UiRuntime`] alone, without pulling in `winit`/`softbuffer`/
//! `notify` at all, can disable it (`default-features = false`).

mod components;
mod focus;
mod position_observer;
mod runtime;
mod scroll;
mod size_observer;
mod text_input;
mod viewport;
mod virtual_list;

pub use components::dialog::{Dialog, DialogProps, MODAL_ROOT_CLASS};
pub use components::popover::{
    Align, POPOVER_ROOT_CLASS, POPOVER_TRIGGER_CLASS, Placement, Popover, PopoverProps, Side,
};
pub use components::portal::{Portal, PortalProps};
pub use components::slider::{
    SLIDER_CLASS, SLIDER_INPUT_CLASS, SLIDER_THUMB_CLASS, Slider, SliderProps, SliderValue,
};
pub use components::switch::{
    SWITCH_CLASS, SWITCH_INPUT_CLASS, SWITCH_THUMB_CLASS, Switch, SwitchProps, SwitchValue,
};
pub use position_observer::{PositionObserverRegistry, use_committed_position};
pub use runtime::UiRuntime;
pub use scroll::{ScrollHandle, ScrollRegistry, use_scroll_offset};
pub use size_observer::{SizeObserverRegistry, use_committed_size};
pub use viewport::{ViewportSize, use_viewport_size};
pub use virtual_list::{ItemHeight, Overscan, VirtualListHandle, use_virtual_list};

#[cfg(feature = "desktop")]
mod clipboard;

#[cfg(feature = "desktop")]
mod desktop;

#[cfg(feature = "desktop")]
pub use desktop::{
    RunError, RunOutcome, WindowOptions, WindowSpec, run, run_single_instance, run_windows,
    run_with_css_reload, run_with_css_reload_and_options, run_with_options,
};

#[cfg(feature = "desktop")]
mod activation;

#[cfg(feature = "desktop")]
pub use activation::{
    ActivationEvent, ActivationEvents, SingleInstance, classify_launch,
    probe_single_instance_capability, use_activation_events,
};

#[cfg(feature = "desktop")]
mod single_instance;

#[cfg(feature = "desktop")]
mod drag_drop;

#[cfg(feature = "desktop")]
pub use drag_drop::{DragEvent, DragPayload, DragPosition};

#[cfg(feature = "desktop")]
mod file_dialog;

#[cfg(feature = "desktop")]
pub use file_dialog::{
    FileDialogFilter, OpenFileDialogOptions, OpenFileDialogOutcome, SaveFileDialogOptions,
    SaveFileDialogOutcome,
};

#[cfg(feature = "desktop")]
pub mod accessibility;

#[cfg(feature = "desktop")]
pub mod appearance;

#[cfg(feature = "desktop")]
pub mod caption;

#[cfg(feature = "desktop")]
pub mod dpi;

#[cfg(feature = "desktop")]
pub mod gpu;

#[cfg(feature = "desktop")]
mod menu;

#[cfg(feature = "desktop")]
pub use menu::{ContextMenuOutcome, MenuCommandId, MenuEntry};

#[cfg(feature = "desktop")]
mod os;

#[cfg(feature = "desktop")]
pub mod overlay;

#[cfg(feature = "desktop")]
pub mod theme;

#[cfg(feature = "desktop")]
pub mod tray;

#[cfg(feature = "desktop")]
mod window_controls;

#[cfg(feature = "desktop")]
mod window_state;

#[cfg(feature = "desktop")]
pub use window_state::{WindowPersistence, probe_persistence_capability, reset_window_state};

#[cfg(feature = "desktop")]
pub use window_controls::{
    InputMode, WINDOW_DRAG_REGION_ID, WINDOW_INPUT_REGION_CLASS, WindowControls,
    use_window_controls,
};
