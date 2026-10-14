//! An isolated feasibility experiment, separate from
//! `florui_platform::appearance`'s own committed contract: does a real
//! `wgpu` surface actually composite alpha with the desktop on this
//! machine, where `softbuffer` (see `appearance_probe`'s own doc)
//! provably cannot? This does not change what `appearance_probe` reports
//! and nothing here is wired into `florui_platform::desktop` — it exists
//! to gather real evidence before any of that.
//!
//! `wgpu` supporting alpha in principle doesn't mean this machine's
//! actual adapter/surface/compositor combination does —
//! [`SurfaceCapabilities::alpha_modes`] is queried and printed for real,
//! not assumed, and picking `CompositeAlphaMode::PostMultiplied`/
//! `PreMultiplied` when the list only ever offers `Opaque` would just
//! silently fall back to opaque presentation, the same trap
//! `appearance_probe`'s own `Attempted`-vs-verified distinction exists to
//! avoid.
//!
//! # A real, verified finding: real Windows transparency through `wgpu`, with an exact recipe
//!
//! This module's own first draft concluded `wgpu`'s standard surface
//! couldn't get real alpha compositing on Windows at all, having only
//! tried `wgpu`'s *default* DX12 configuration. That conclusion was
//! wrong, caught by re-checking against `wgpu`'s own source rather than
//! trusting the first negative result: `wgpu` 29.0.4 has a *second*,
//! non-default DX12 presentation path,
//! [`Dx12SwapchainKind::DxgiFromVisual`] (the default,
//! `DxgiFromHwnd`, is documented — in `wgpu` itself — as not supporting
//! transparency at all; `DxgiFromVisual` is the one documented as
//! supporting it, via a DirectComposition visual `wgpu` creates
//! automatically). Getting a real transparent, alpha-composited window
//! actually working end to end — confirmed by a human looking at the
//! real desktop showing through, at both partial and full transparency,
//! not just an API call returning success — took every one of these,
//! together, on real discrete-GPU Windows hardware:
//!
//! - `Backends::DX12` specifically (`Backends::VULKAN` reports
//!   `alpha_modes: [Opaque]` and nothing else on the same machine, a
//!   finding that still stands).
//! - `backend_options.dx12.presentation_system =
//!   Dx12SwapchainKind::DxgiFromVisual`, not `wgpu`'s own default.
//! - **Both** `WindowAttributes::with_transparent(true)` **and** the
//!   separate, Windows-specific
//!   `WindowAttributesExtWindows::with_no_redirection_bitmap(true)`.
//!   Without the second one, `Surface::configure` still succeeds and
//!   nothing errors anywhere — the window just paints solid color with
//!   no desktop showing through, because the window's own default
//!   redirection bitmap stays opaque behind whatever DirectComposition
//!   draws. This exact silent failure mode is what made this module's
//!   first (wrong) conclusion look confirmed.
//! - `CompositeAlphaMode::PreMultiplied`, not `PostMultiplied` — both are
//!   listed in `alpha_modes`, but selecting `PostMultiplied` makes
//!   `Surface::configure` panic with a `wgpu` validation error
//!   ("Invalid surface") in this exact DX12 DirectComposition path, a
//!   real `wgpu` 29.0.4 limitation distinct from anything about Windows
//!   or this crate. Clear colors have to actually be premultiplied to
//!   match (`(r*a, g*a, b*a, a)`), which [`ClearState::clear_color`]
//!   does.
//!
//! With all of that, `TransparentSurface` genuinely is achievable on
//! Windows through `wgpu` — this crate's earlier, broader claim that
//! switching backends "wouldn't deliver it either" was itself wrong, not
//! just under-evidenced. Metal (macOS) and Vulkan-on-Wayland/X11
//! (Linux) each need their own from-scratch verification along these
//! same lines — nothing here transfers across platforms by assumption,
//! per [`Dx12SwapchainKind`] being exactly the kind of
//! platform-specific, non-obvious detail a *different* backend's own
//! "obvious" default configuration could just as easily be hiding.
//!
//! `cargo run --example gpu_transparency_probe -p florui-platform`
//!
//! Press Space to cycle the whole window through three states — opaque
//! red, 50%-alpha green, and fully transparent (alpha `0.0`) — and
//! actually look at each one against whatever's behind this window on
//! the real desktop. Also resize the window and move it to a
//! different-DPI display if one is available, watching stdout for what
//! this probe observes.
//!
//! `wgpu` picks a default backend (Vulkan, on most real Windows
//! machines) unless told otherwise — set `FLORUI_GPU_BACKEND` to
//! `vulkan`, `dx12`, or `gl` to force a specific one; the working recipe
//! above only applies to `dx12`. `WGPU_DX12_PRESENTATION_SYSTEM=hwnd`
//! (a real `wgpu` env var this probe already respects) reproduces the
//! original, non-transparent default for a side-by-side comparison.

use std::num::NonZeroU32;
use std::sync::Arc;

use wgpu::{CompositeAlphaMode, PresentMode, TextureUsages};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowId};

#[derive(Debug, Clone, Copy)]
enum ClearState {
    OpaqueRed,
    HalfAlphaGreen,
    FullyTransparent,
}

impl ClearState {
    fn next(self) -> Self {
        match self {
            ClearState::OpaqueRed => ClearState::HalfAlphaGreen,
            ClearState::HalfAlphaGreen => ClearState::FullyTransparent,
            ClearState::FullyTransparent => ClearState::OpaqueRed,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ClearState::OpaqueRed => "opaque red — should look solid either way",
            ClearState::HalfAlphaGreen => {
                "50%-alpha green — should show half the desktop through it, if alpha \
                 compositing genuinely works"
            }
            ClearState::FullyTransparent => {
                "fully transparent (alpha 0.0) — should show the desktop with no tint at \
                 all, if alpha compositing genuinely works"
            }
        }
    }

    /// This state's own straight (non-premultiplied) color/alpha.
    fn straight(self) -> (f64, f64, f64, f64) {
        match self {
            ClearState::OpaqueRed => (1.0, 0.0, 0.0, 1.0),
            ClearState::HalfAlphaGreen => (0.0, 1.0, 0.0, 0.5),
            ClearState::FullyTransparent => (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// The clear color to actually submit, given which
    /// `CompositeAlphaMode` the surface actually negotiated —
    /// `CompositeAlphaMode::PreMultiplied` expects color channels already
    /// multiplied by alpha; `PostMultiplied` (and `Opaque`, where alpha
    /// is ignored either way) expects the straight values as-is. Getting
    /// this wrong doesn't fail loudly — it just renders too dark, which
    /// would look identical to "alpha compositing isn't working" and
    /// silently invalidate this probe's own visual test.
    fn clear_color(self, alpha_mode: CompositeAlphaMode) -> wgpu::Color {
        let (r, g, b, a) = self.straight();
        match alpha_mode {
            CompositeAlphaMode::PreMultiplied => wgpu::Color {
                r: r * a,
                g: g * a,
                b: b * a,
                a,
            },
            _ => wgpu::Color { r, g, b, a },
        }
    }
}

struct GpuWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    alpha_mode: CompositeAlphaMode,
}

struct App {
    gpu: Option<GpuWindow>,
    state: ClearState,
}

impl App {
    fn redraw(&mut self) {
        let Some(gpu) = &self.gpu else {
            return;
        };
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear-only probe pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.state.clear_color(gpu.alpha_mode)),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        gpu.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn reconfigure(&mut self, width: u32, height: u32) {
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        let (Some(width), Some(height)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return;
        };
        gpu.config.width = width.get();
        gpu.config.height = height.get();
        gpu.surface.configure(&gpu.device, &gpu.config);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title(
                "gpu_transparency_probe — Space cycles states, watch stdout and this window",
            )
            .with_transparent(true);
        // Windows-specific, and separate from `with_transparent`
        // above: without `WS_EX_NOREDIRECTIONBITMAP`, the window's
        // own default redirection bitmap stays opaque behind
        // whatever DirectComposition draws, silently defeating
        // per-pixel alpha even when the surface itself configures
        // without error. Missing this exact flag is what made the
        // very first pass of this probe (`with_transparent(true)`
        // alone, `Surface::configure` succeeding either way) show
        // solid color with no desktop showing through.
        #[cfg(windows)]
        let attrs = attrs.with_no_redirection_bitmap(true);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("window creation should not fail on a real desktop session"),
        );

        let mut backend_options = wgpu::BackendOptions::from_env_or_default();
        // `Dx12SwapchainKind::DxgiFromHwnd` (`wgpu`'s own default) is
        // documented, in `wgpu` itself, as not supporting transparency at
        // all — this crate's own first probe run used that default
        // without knowing it, and wrongly read `alpha_modes: [Opaque]`
        // there as "DX12 itself can't do this." `DxgiFromVisual` is the
        // one `wgpu` itself documents as supporting transparent windows
        // (via an automatically-created DirectComposition visual) — see
        // this module's own doc for what actually happened once this got
        // corrected. Only overriding when `WGPU_DX12_PRESENTATION_SYSTEM`
        // (`wgpu`'s own env var) wasn't already set, so it can still
        // force `hwnd` back for a side-by-side comparison.
        if wgpu::Dx12SwapchainKind::from_env().is_none() {
            backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        }
        println!(
            "DX12 presentation system (only takes effect if the DX12 backend is actually \
             selected): {:?}",
            backend_options.dx12.presentation_system
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: requested_backends(),
            backend_options,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // An owned `Arc<Window>` clone, not a borrow — lets `wgpu` give
        // back a real `Surface<'static>` on its own, no unsafe lifetime
        // extension needed for this struct to hold both.
        let surface = instance
            .create_surface(window.clone())
            .expect("surface creation should not fail on a real desktop session");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("a compatible GPU adapter should exist on a real desktop session");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("gpu_transparency_probe device"),
            ..Default::default()
        }))
        .expect("device request should not fail on a real desktop session");

        let capabilities = surface.get_capabilities(&adapter);
        println!("adapter: {:?}", adapter.get_info());
        println!(
            "surface alpha modes actually reported: {:?}",
            capabilities.alpha_modes
        );
        println!(
            "surface formats actually reported: {:?}",
            capabilities.formats
        );

        // Prefer real per-pixel compositing if the surface genuinely
        // offers it; report plainly when it only offers `Opaque` (or
        // `Inherit`, which `wgpu`'s own doc notes behaves like `Opaque`
        // on most backends) — picking a mode the list doesn't contain
        // would just silently fall back, hiding exactly the finding this
        // probe exists to surface.
        let alpha_mode = capabilities
            .alpha_modes
            .contains(&CompositeAlphaMode::PreMultiplied)
            .then_some(CompositeAlphaMode::PreMultiplied)
            .or_else(|| {
                capabilities
                    .alpha_modes
                    .contains(&CompositeAlphaMode::PostMultiplied)
                    .then_some(CompositeAlphaMode::PostMultiplied)
            })
            .unwrap_or(capabilities.alpha_modes[0]);
        println!("alpha mode this probe picked: {alpha_mode:?}");
        if !matches!(
            alpha_mode,
            CompositeAlphaMode::PostMultiplied | CompositeAlphaMode::PreMultiplied
        ) {
            println!(
                "no real per-pixel alpha mode was available at all — this window can only \
                 ever present opaque, regardless of what's cleared into it"
            );
        }

        let size = window.inner_size();
        // Non-sRGB, in case DirectComposition alpha blending and an sRGB
        // view don't combine validly (untested hypothesis — falls back
        // to whatever's first if no plain Unorm variant is listed).
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| !format!("{f:?}").contains("Srgb"))
            .unwrap_or(capabilities.formats[0]);
        println!("format this probe picked: {format:?}");
        let config = wgpu::SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        self.gpu = Some(GpuWindow {
            window,
            surface,
            device,
            queue,
            config,
            alpha_mode,
        });
        println!(
            "scale factor: {}",
            self.gpu.as_ref().unwrap().window.scale_factor()
        );
        println!("state: {}", self.state.label());
        self.redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gpu) = &self.gpu else { return };
        if gpu.window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                println!("resized to {}x{} physical px", size.width, size.height);
                self.reconfigure(size.width, size.height);
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                println!("scale factor changed to {scale_factor}");
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Space) =>
            {
                self.state = self.state.next();
                println!("state: {}", self.state.label());
                self.redraw();
            }
            _ => {}
        }
    }
}

/// `FLORUI_GPU_BACKEND` (`vulkan`/`dx12`/`gl`), or `wgpu`'s own default
/// (`Backends::PRIMARY` — Vulkan, Metal, DX12, and browser WebGPU) when
/// unset or unrecognized.
fn requested_backends() -> wgpu::Backends {
    match std::env::var("FLORUI_GPU_BACKEND").as_deref() {
        Ok("vulkan") => wgpu::Backends::VULKAN,
        Ok("dx12") => wgpu::Backends::DX12,
        Ok("gl") => wgpu::Backends::GL,
        _ => wgpu::Backends::default(),
    }
}

fn main() {
    let event_loop = EventLoop::new().expect("event loop should build on a real desktop session");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        gpu: None,
        state: ClearState::OpaqueRed,
    };
    event_loop
        .run_app(&mut app)
        .expect("event loop should not fail on a real desktop session");
}
