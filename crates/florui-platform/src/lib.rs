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
//!
//! # Tests
//!
//! Two tiers. `cargo test` runs the headless ones: a [`UiRuntime`] driven
//! directly, no window or OS service. Tests that need a real Windows session
//! (the single-instance mutex, the application-data folder) are `#[ignore]`d
//! and run with `cargo test -p florui-platform -- --ignored`, as CI does on a
//! Windows runner. What needs real windows, mouse and keyboard input is
//! driven through UI Automation by the scripts in `scripts/real-session/`.
//!
//! What each control does is published in `CONTROLS.md`, next to this crate's
//! `Cargo.toml`.

mod auto_id;
mod components;
mod focus;
mod focus_observer;
mod form;
mod form_control;
mod icon;
mod image;
mod list_keys;
mod menu_keys;
mod position_observer;
mod runtime;
mod scroll;
mod scroll_animation;
mod select;
mod size_observer;
#[cfg(all(test, feature = "desktop", target_os = "windows"))]
mod test_event_loop;
mod text_input;
mod textarea_resize;
mod validation_bubble;
mod validation_message;
mod viewport;
mod virtual_list;
mod visited_links;

pub use components::dialog::{Dialog, DialogProps, MODAL_ROOT_CLASS};
pub use components::popover::{
    Align, POPOVER_ROOT_CLASS, POPOVER_TRIGGER_CLASS, Placement, Popover, PopoverProps, Side,
};
pub use components::portal::{Portal, PortalProps};
pub use components::slider::{
    SLIDER_CLASS, SLIDER_FILL_CLASS, SLIDER_INPUT_CLASS, SLIDER_THUMB_CLASS, Slider, SliderProps,
    SliderValue,
};
pub use components::switch::{
    SWITCH_CLASS, SWITCH_INPUT_CLASS, SWITCH_THUMB_CLASS, Switch, SwitchProps, SwitchValue,
};
pub use focus_observer::{
    FocusController, FocusObserverRegistry, use_focus_controller, use_focus_within,
};
pub use list_keys::ListKeyRegistry;
pub use position_observer::{PositionObserverRegistry, use_committed_position};
pub use runtime::UiRuntime;
pub use scroll::{ScrollHandle, ScrollRegistry, use_scroll_offset};
pub use select::SELECT_OPTIONS_CLASS;
pub use size_observer::{SizeObserverRegistry, use_committed_size};
pub use validation_bubble::{
    BUBBLE_ARROW_CLASS, BUBBLE_CLASS, BUBBLE_ICON_CLASS, BUBBLE_ICON_DOT_CLASS,
    BUBBLE_ICON_STEM_CLASS, BUBBLE_TEXT_CLASS,
};
pub use viewport::{ViewportSize, use_viewport_size};
pub use virtual_list::{ItemHeight, Overscan, VirtualListHandle, use_virtual_list};
pub use visited_links::VisitedLinks;

#[cfg(feature = "desktop")]
mod clipboard;

#[cfg(feature = "desktop")]
mod desktop;

#[cfg(feature = "desktop")]
mod host_observer;

#[cfg(feature = "desktop")]
mod frame;

#[cfg(feature = "desktop")]
mod headless;

#[cfg(feature = "desktop")]
pub use headless::{A11yNode, HeadlessFrame, HeadlessOptions, HeadlessWindow, TestKey};

#[cfg(feature = "desktop")]
mod input;

#[cfg(feature = "desktop")]
pub use clipboard::{ClipboardAccess, MemoryClipboard};

#[cfg(feature = "desktop")]
pub use host_observer::{HostObserver, ObservedFrame, OverlayCanvas};

#[cfg(feature = "desktop")]
pub use desktop::{
    RunError, RunOutcome, WindowOptions, WindowSpec, run, run_single_instance, run_windows,
    run_windows_observed, run_with_css_reload, run_with_css_reload_and_options, run_with_options,
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
mod href;

#[cfg(feature = "desktop")]
mod open_url;

#[cfg(feature = "desktop")]
pub use open_url::OpenUrlOutcome;

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
