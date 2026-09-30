//! Real GPU-backed window presentation for [`crate::desktop`]'s
//! `DesktopHost`, preferred over `softbuffer` whenever a real adapter
//! comes up — `softbuffer` stays the deliberate CPU fallback when it
//! doesn't, not a path this crate is dropping. Painting itself is
//! unchanged either way: [`florui_paint::paint_to_buffer`] still
//! rasterizes on the CPU exactly as before; only *how the resulting
//! pixels reach the screen* differs. Uploading a CPU-painted buffer to
//! the GPU every frame has a real cost this module doesn't eliminate —
//! it only buys real desktop-compositor alpha, not faster painting; a
//! true GPU-rendered paint pipeline is a separate, later migration.
//!
//! # Two separate questions, not one
//!
//! "Can this host render through the GPU at all?" and "can it also
//! present a genuinely transparent window?" are independent — failing
//! the second must not force falling back on the first. That's
//! [`PresentationCapability`]'s own three states, decided once per
//! window and never silently downgraded later:
//!
//! - [`PresentationCapability::GpuTransparent`]: a real adapter/device
//!   came up *and* the surface negotiated genuine per-pixel alpha
//!   compositing — the exact `Backends::DX12` +
//!   `Dx12SwapchainKind::DxgiFromVisual` +
//!   `with_no_redirection_bitmap(true)` + `CompositeAlphaMode::PreMultiplied`
//!   recipe the `gpu_transparency_probe` example (see its own doc)
//!   confirmed live, human-verified against the real desktop, not just
//!   `Surface::configure` returning success.
//! - [`PresentationCapability::GpuOpaque`]: a real adapter/device came
//!   up, but no real alpha compositing mode did (or wasn't attempted) —
//!   still GPU-presented, just opaque. This is not a failure case:
//!   GPU rendering with an opaque window is strictly better than
//!   falling all the way back to the CPU path over a decorative
//!   capability alone.
//! - [`PresentationCapability::Cpu`]: no usable GPU adapter came up at
//!   all — the same `softbuffer` presentation this crate already used
//!   everywhere, unchanged, and still opaque-only (see
//!   `crate::appearance`'s own doc for why `softbuffer` itself can
//!   never do per-pixel alpha).
//!
//! No dedicated GPU is required for either GPU-backed capability — an
//! integrated GPU with the right adapter/driver features is enough, and
//! "DirectX 12 is installed" alone doesn't guarantee that; the real
//! adapter/driver combination decides, which is exactly why this is
//! probed live against a real window rather than assumed from the OS
//! version.
//!
//! # What's validated here, and what isn't yet
//!
//! Run live end to end through `crate::desktop::DesktopHost` (the
//! `counter` example, `cargo run --example counter -p florui-example-app`),
//! not just this module's own probe: a real window came up through
//! [`PresentationCapability::GpuTransparent`] on this machine, survived a
//! live resize (816×639 to 1200×800, reconfigured and repainted correctly
//! with no distortion or crash), correctly dispatched a real click through
//! `DesktopHost`'s own hit-testing and `Signal` update (the counter's
//! value and its derived `x2` both advanced, proving layout, paint, and
//! presentation all stayed in sync through this path), and survived a
//! real minimize/restore cycle (process stayed responsive throughout,
//! resumed rendering correctly afterward, no repeated
//! `"could not present the frame"` errors). Painted content itself was
//! opaque throughout that run (the example's own `canvas_color`); the
//! presentation layer's real alpha capability is proven, but no example
//! has yet passed a non-opaque `canvas_color` through `DesktopHost` to
//! confirm an actually see-through *application* window end to end — the
//! next real thing to verify, not yet done.
//!
//! [`GpuPresenter::present`] reconfigures and retries once on a real
//! `Outdated` surface (confirmed live: resizing the window down to
//! near-zero and back through `DesktopHost` recovers cleanly, no panic,
//! no stuck stale-frame state) and skips a frame that cannot be shown
//! (timeout, occluded window). A lost device or surface is reported as
//! [`PresentOutcome::Lost`]; the desktop host then builds a new presenter,
//! the hardware once more and, if that is lost too, `wgpu`'s software
//! adapter. `softbuffer` is only the last resort: on Windows a window made
//! for transparent composition (no redirection bitmap) shows nothing drawn
//! with it, measured by forcing it at startup. A test destroys a real device
//! to cover the presenter side; the host side was run the same way by hand,
//! with the device destroyed twice under a running window. Memory footprint was not measured against an
//! automated budget; color/alpha fidelity was checked by eye against
//! real screenshots (this module's own probe and the `counter` run
//! above), not an automated reference image.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use wgpu::{
    CompositeAlphaMode, Extent3d, PresentMode, TexelCopyBufferLayout, TexelCopyTextureInfo,
    TextureAspect, TextureDimension, TextureFormat, TextureUsages,
};
use winit::window::Window;

/// See this module's own doc for what each state means and the fallback
/// contract between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationCapability {
    GpuTransparent,
    GpuOpaque,
    Cpu,
}

/// Adds the window attributes a GPU-transparent presentation attempt
/// needs, on top of whatever `attrs` the caller already built — always
/// safe to call even when [`GpuPresenter::try_new`] ends up choosing
/// [`PresentationCapability::GpuOpaque`] or
/// [`PresentationCapability::Cpu`] instead: a transparent-*capable*
/// window whose content is always painted fully opaque (this crate's
/// own default, unchanged) looks identical to a plain opaque window
/// either way, and `softbuffer`'s own GDI `BitBlt` presentation (see
/// `crate::appearance`'s own doc) draws to the window's own device
/// context directly, not through DWM's redirection surface — the one
/// `with_no_redirection_bitmap` disables — so the CPU fallback path
/// keeps working under these same attributes too.
pub fn transparent_capable_attributes(
    attrs: winit::window::WindowAttributes,
) -> winit::window::WindowAttributes {
    let attrs = attrs.with_transparent(true);
    #[cfg(target_os = "windows")]
    let attrs = crate::os::windows::gpu::transparent_window_attributes(attrs);
    attrs
}

pub struct GpuPresenter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    capability: PresentationCapability,
    upload_texture: wgpu::Texture,
    upload_size: (u32, u32),
    /// Set by wgpu when the device is lost or destroyed.
    device_lost: Arc<AtomicBool>,
}

/// What [`GpuPresenter::present`] did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented,
    /// Nothing was shown this time (timeout, occluded window, stale surface
    /// that did not recover); the next frame may work.
    Skipped,
    /// The device or surface is gone for good: this presenter never shows
    /// another frame and the caller must build a new one.
    Lost,
}

impl GpuPresenter {
    pub fn capability(&self) -> PresentationCapability {
        self.capability
    }

    /// The real surface format negotiated for this presenter — evidence
    /// for `florui doctor --presentation` (see its own doc), not just an
    /// internal implementation detail.
    pub fn format(&self) -> TextureFormat {
        self.config.format
    }

    /// The real alpha-compositing mode negotiated for this presenter —
    /// `CompositeAlphaMode::Opaque` for [`PresentationCapability::GpuOpaque`],
    /// `PreMultiplied` for [`PresentationCapability::GpuTransparent`] (see
    /// this module's own doc for why `PreMultiplied` specifically).
    pub fn alpha_mode(&self) -> CompositeAlphaMode {
        self.config.alpha_mode
    }

    /// Tries every presentation path in order, from most to least
    /// capable, returning the first that actually comes up on a real
    /// adapter — `None` only when no GPU adapter could be obtained at
    /// all, the signal `crate::desktop::DesktopHost` uses to fall back
    /// to `softbuffer`.
    pub fn try_new(window: Arc<Window>) -> Option<Self> {
        #[cfg(target_os = "windows")]
        if let Some(presenter) =
            crate::os::windows::gpu::try_dx12_direct_composition(window.clone())
        {
            return Some(presenter);
        }
        Self::try_default_opaque(window.clone(), false).or_else(|| Self::try_new_software(window))
    }

    /// Only `wgpu`'s software adapter, opaque. Used after a hardware device
    /// was lost twice, and as the last resort in [`Self::try_new`]. It stays
    /// on `wgpu` because a window made for transparent composition shows
    /// nothing drawn with `softbuffer`.
    pub fn try_new_software(window: Arc<Window>) -> Option<Self> {
        Self::try_default_opaque(window, true)
    }

    /// Whatever backend `wgpu` picks by default, opaque only — the
    /// second-tier fallback: still GPU-rendered, just without real
    /// per-pixel alpha compositing. `software` restricts it to the software
    /// adapter.
    fn try_default_opaque(window: Arc<Window>, software: bool) -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).ok()?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            force_fallback_adapter: software,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("florui-platform GPU presenter (opaque)"),
            ..Default::default()
        }))
        .ok()?;

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, TextureFormat::Rgba8Unorm))
            .or_else(|| capabilities.formats.first().copied())?;

        Self::configure(
            window,
            device,
            queue,
            surface,
            format,
            CompositeAlphaMode::Opaque,
            PresentationCapability::GpuOpaque,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn configure(
        window: Arc<Window>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        format: TextureFormat,
        alpha_mode: CompositeAlphaMode,
        capability: PresentationCapability,
    ) -> Option<Self> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);
        let config = wgpu::SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST,
            format,
            width,
            height,
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let upload_texture = create_upload_texture(&device, format, width, height);
        let device_lost = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&device_lost);
        device.set_device_lost_callback(move |_, _| flag.store(true, Ordering::SeqCst));

        Some(Self {
            device,
            queue,
            surface,
            config,
            capability,
            upload_texture,
            upload_size: (width, height),
            device_lost,
        })
    }

    /// `true` once wgpu reported the device lost or destroyed. wgpu only
    /// runs that callback when the device is polled, so this polls first.
    pub fn is_lost(&self) -> bool {
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.device_lost.load(Ordering::SeqCst)
    }

    /// Reconfigures the surface (and its upload texture) for a new
    /// physical size — the GPU-path equivalent of `softbuffer::Surface::resize`,
    /// called from the same resize/scale-factor-changed handling
    /// `crate::desktop::DesktopHost` already had.
    pub fn resize(&mut self, width: u32, height: u32) {
        // Every call on a lost device is a validation error, which panics.
        if self.is_lost() {
            return;
        }
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == (self.config.width, self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.upload_texture =
            create_upload_texture(&self.device, self.config.format, width, height);
        self.upload_size = (width, height);
    }

    /// Uploads `rgba` (tiny-skia's own premultiplied `R,G,B,A` byte
    /// order — matches this presenter's own `Rgba8Unorm` format
    /// directly, no channel-swizzling needed) and presents it. `rgba`
    /// must be exactly `width * height * 4` bytes for the size last
    /// passed to [`Self::resize`] (or the size at construction, if never
    /// resized) — a mismatch is a caller bug, not a recoverable runtime
    /// condition, so this asserts rather than silently corrupting the
    /// frame.
    ///
    /// An `Outdated` surface reconfigures and retries once; a timed-out,
    /// occluded or invalid surface, or a retry that still fails, skips this
    /// frame. A lost device or surface reports [`PresentOutcome::Lost`] so the
    /// caller can build a new presenter.
    pub fn present(&mut self, rgba: &[u8]) -> PresentOutcome {
        if self.is_lost() {
            return PresentOutcome::Lost;
        }
        let (width, height) = self.upload_size;
        assert_eq!(
            rgba.len(),
            (width as usize) * (height as usize) * 4,
            "present() called with a buffer that doesn't match this presenter's own last-known \
             size — the caller must resize() before presenting a differently-sized frame"
        );

        self.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &self.upload_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            // `Outdated` means the surface itself is stale but the device
            // and adapter are still fine -- wgpu's own documented
            // recovery is reconfigure-and-retry. A plain `self.resize()`
            // call would wrongly no-op here on an unchanged `(width,
            // height)` (its own guard is for the ordinary resize-event
            // path, not this one), so this reconfigures unconditionally
            // instead.
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(texture)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
                    wgpu::CurrentSurfaceTexture::Lost => return PresentOutcome::Lost,
                    _ => return PresentOutcome::Skipped,
                }
            }
            // A new `Surface` is needed, which only the caller can make.
            wgpu::CurrentSurfaceTexture::Lost => return PresentOutcome::Lost,
            _ => return PresentOutcome::Skipped,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("florui-platform GPU presenter upload blit"),
            });
        encoder.copy_texture_to_texture(
            TexelCopyTextureInfo {
                texture: &self.upload_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            TexelCopyTextureInfo {
                texture: &frame.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        PresentOutcome::Presented
    }

    /// Destroys the device, as a driver reset would lose it.
    #[cfg(test)]
    pub(crate) fn lose_device(&self) {
        self.device.destroy();
    }
}

/// Real evidence from a windowless GPU-capability probe — no surface, so
/// this cannot say anything about presentation or transparency (see
/// [`PresentationCapability`] for that); only whether *some* real adapter
/// and device come up at all, and which one. `florui doctor --graphics`
/// runs this in its own bounded process (see that command's own doc) so a
/// crashed or hung driver only takes down the probe, not the whole
/// `doctor` run.
#[derive(Debug, Clone)]
pub struct GraphicsProbe {
    pub backend: String,
    pub adapter_name: String,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
}

/// Requests any adapter (`compatible_surface: None`, since there is no
/// window here) and a device from it, returning real, observed
/// information about whichever one actually came up — never a guess from
/// the OS/driver being merely installed.
pub fn probe_graphics() -> Result<GraphicsProbe, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: None,
        ..Default::default()
    }))
    .map_err(|error| format!("no usable graphics adapter: {error}"))?;
    let info = adapter.get_info();
    // The device request itself is real evidence too -- an adapter can be
    // enumerated but still fail to actually produce a device (a driver
    // that lies about its own capabilities, a resource limit), so this
    // probe does not report success on enumeration alone.
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("florui doctor graphics probe"),
        ..Default::default()
    }))
    .map_err(|error| format!("adapter enumerated but device request failed: {error}"))?;

    Ok(GraphicsProbe {
        backend: format!("{:?}", info.backend),
        adapter_name: info.name,
        device_type: format!("{:?}", info.device_type),
        driver: info.driver,
        driver_info: info.driver_info,
    })
}

fn create_upload_texture(
    device: &wgpu::Device,
    format: TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("florui-platform GPU presenter upload texture"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::COPY_SRC | TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[cfg(all(test, feature = "desktop", target_os = "windows"))]
mod tests {
    use std::time::Duration;

    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::ActiveEventLoop;
    use winit::window::{Window, WindowId};

    use crate::desktop::UserEvent;

    use super::*;

    #[derive(Default)]
    struct App {
        window: Option<Arc<Window>>,
    }

    impl ApplicationHandler<UserEvent> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let attributes =
                Window::default_attributes().with_inner_size(winit::dpi::PhysicalSize::new(64, 64));
            self.window = event_loop.create_window(attributes).ok().map(Arc::new);
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }

    fn frame(presenter: &mut GpuPresenter) -> PresentOutcome {
        let (width, height) = presenter.upload_size;
        presenter.present(&vec![0x40; (width * height * 4) as usize])
    }

    /// `destroy()` reports the loss on a later poll, not on the next one: on a
    /// slow adapter a single poll right after it can still say "not lost".
    fn wait_until_lost(presenter: &GpuPresenter) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if presenter.is_lost() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        presenter.is_lost()
    }

    #[test]
    #[ignore = "needs a real Windows session: cargo test -p florui-platform -- --ignored"]
    fn a_lost_device_is_reported_and_a_new_presenter_works() {
        crate::test_event_loop::on_event_loop(|event_loop| {
            let mut app = App::default();
            crate::test_event_loop::pump(event_loop, &mut app, Duration::ZERO);
            let window = app.window.clone().expect("the window opens");

            let Some(mut presenter) = GpuPresenter::try_new(window.clone()) else {
                eprintln!("no GPU adapter on this machine; nothing to lose");
                return;
            };
            assert_ne!(frame(&mut presenter), PresentOutcome::Lost);

            presenter.lose_device();
            assert!(
                wait_until_lost(&presenter),
                "wgpu reports the destroyed device"
            );
            assert_eq!(frame(&mut presenter), PresentOutcome::Lost);
            presenter.resize(128, 128);
            assert_eq!(
                frame(&mut presenter),
                PresentOutcome::Lost,
                "a lost presenter stays lost and never panics"
            );

            drop(presenter);
            let mut rebuilt =
                GpuPresenter::try_new(window.clone()).expect("a new presenter comes up");
            assert!(!rebuilt.is_lost());
            assert_ne!(frame(&mut rebuilt), PresentOutcome::Lost);

            // The software adapter is the next level down after a second loss.
            drop(rebuilt);
            if let Some(mut software) = GpuPresenter::try_new_software(window) {
                assert!(!software.is_lost());
                assert_ne!(frame(&mut software), PresentOutcome::Lost);
            } else {
                eprintln!("no software adapter on this machine");
            }
        });
    }
}
