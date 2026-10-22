//! The DX12 + DirectComposition presentation path -- the one real,
//! live-verified route to genuine per-pixel window alpha. See
//! `crate::gpu`'s own doc for why this is tried first and what falls
//! back to when it doesn't come up.

use std::sync::Arc;

use wgpu::{CompositeAlphaMode, TextureFormat};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes};

use crate::GpuMemory;
use crate::gpu::{GpuPresenter, PresentationCapability};

/// See `crate::gpu::transparent_capable_attributes`'s own doc for why
/// this is always safe to apply even when the DX12 path below ends up
/// unused.
pub(crate) fn transparent_window_attributes(attrs: WindowAttributes) -> WindowAttributes {
    attrs.with_no_redirection_bitmap(true)
}

/// `Dx12SwapchainKind::DxgiFromVisual` + `CompositeAlphaMode::PreMultiplied`
/// -- see `crate::gpu`'s own doc for why only this exact combination is
/// live-verified. `None` falls through to the default opaque path.
pub(crate) fn try_dx12_direct_composition(
    window: Arc<Window>,
    memory: GpuMemory,
) -> Option<GpuPresenter> {
    let mut backend_options = wgpu::BackendOptions::from_env_or_default();
    if wgpu::Dx12SwapchainKind::from_env().is_none() {
        backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let surface = instance.create_surface(window.clone()).ok()?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("florui-platform GPU presenter (DX12 + DirectComposition)"),
        memory_hints: memory.hints(),
        ..Default::default()
    }))
    .ok()?;

    let capabilities = surface.get_capabilities(&adapter);
    if !capabilities
        .alpha_modes
        .contains(&CompositeAlphaMode::PreMultiplied)
    {
        // `PostMultiplied` is real per-pixel alpha too, but panics
        // `Surface::configure` in this exact DX12 DirectComposition path
        // on `wgpu` 29.0.4 -- see `gpu_transparency_probe`'s own doc.
        // Only `PreMultiplied` is a live-verified working choice;
        // anything else falls through to the opaque path.
        return None;
    }
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|f| matches!(f, TextureFormat::Rgba8Unorm))?;

    GpuPresenter::configure(
        window,
        device,
        queue,
        surface,
        format,
        CompositeAlphaMode::PreMultiplied,
        PresentationCapability::GpuTransparent,
    )
}
