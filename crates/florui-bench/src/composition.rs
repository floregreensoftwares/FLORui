//! What the screen shows and when the compositor presented it, through DXGI
//! desktop duplication of the monitor that holds a point, plus the few window
//! calls the edit-latency measurement needs. Windows only.
//!
//! A duplication frame carries the QPC time the compositor presented it, which
//! is what makes the last interval of the chain measurable on the same clock as
//! the rest. Reading a pixel with GDI instead costs a whole display frame per
//! read and says nothing about when the frame was presented.

use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIFactory1,
    IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, HWND_TOPMOST,
    IsWindowVisible, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
};
use windows::core::{BOOL, Interface, PCWSTR};

use crate::edit_chain::Rgb;

/// QPC ticks now.
pub fn qpc() -> i64 {
    let mut ticks = 0;
    // QueryPerformanceCounter cannot fail on a supported Windows.
    let _ = unsafe { QueryPerformanceCounter(&mut ticks) };
    ticks
}

pub fn qpc_frequency() -> i64 {
    let mut frequency = 0;
    let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
    frequency.max(1)
}

/// Screen coordinates must be physical pixels, as the duplication reports them.
pub fn make_process_dpi_aware() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// A frame the compositor presented, and the color of the watched pixel in it.
#[derive(Debug, Clone, Copy)]
pub struct Presented {
    pub qpc: i64,
    pub color: Rgb,
}

pub struct Composition {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    x: u32,
    y: u32,
    pub refresh_hz: Option<f64>,
}

impl Composition {
    /// Duplicates the monitor that holds the screen point `(x, y)` and watches
    /// that pixel.
    pub fn new(x: i32, y: i32) -> Result<Self, String> {
        unsafe { Self::open(x, y) }.map_err(|e| format!("desktop duplication: {e}"))
    }

    unsafe fn open(px: i32, py: i32) -> windows::core::Result<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let mut chosen = None;
        let mut adapter_index = 0;
        while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
            let mut output_index = 0;
            while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
                let desc = unsafe { output.GetDesc()? };
                let r = desc.DesktopCoordinates;
                if px >= r.left && px < r.right && py >= r.top && py < r.bottom {
                    chosen = Some((adapter.clone(), output.clone(), desc));
                }
                output_index += 1;
            }
            adapter_index += 1;
        }
        let (adapter, output, desc) = chosen.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_INVALIDARG,
                "no monitor holds the window",
            )
        })?;
        let rect = desc.DesktopCoordinates;

        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let refresh_hz = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(desc.DeviceName.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        }
        .as_bool()
        .then_some(f64::from(mode.dmDisplayFrequency))
        .filter(|hz| *hz > 1.0);

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let (device, context) = match (device, context) {
            (Some(device), Some(context)) => (device, context),
            _ => {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "no graphics device",
                ));
            }
        };

        let output1: IDXGIOutput1 = output.cast()?;
        let duplication = unsafe { output1.DuplicateOutput(&device)? };

        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: 1,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
        let staging = staging.ok_or_else(|| {
            windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "no staging texture")
        })?;

        Ok(Self {
            context,
            duplication,
            staging,
            x: (px - rect.left) as u32,
            y: (py - rect.top) as u32,
            refresh_hz,
        })
    }

    /// Waits up to `timeout_ms` for the compositor to present a new frame of
    /// the monitor. `Ok(None)` when it did not, or when the change was only the
    /// pointer.
    pub fn next(&self, timeout_ms: u32) -> Result<Option<Presented>, String> {
        unsafe { self.next_frame(timeout_ms) }.map_err(|e| format!("desktop duplication: {e}"))
    }

    unsafe fn next_frame(&self, timeout_ms: u32) -> windows::core::Result<Option<Presented>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        match unsafe {
            self.duplication
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(e),
        }
        let result = (|| {
            if info.LastPresentTime == 0 {
                return Ok(None);
            }
            let Some(resource) = resource.as_ref() else {
                return Ok(None);
            };
            let texture: ID3D11Texture2D = resource.cast()?;
            let region = D3D11_BOX {
                left: self.x,
                top: self.y,
                front: 0,
                right: self.x + 1,
                bottom: self.y + 1,
                back: 1,
            };
            unsafe {
                self.context.CopySubresourceRegion(
                    &self.staging,
                    0,
                    0,
                    0,
                    0,
                    &texture,
                    0,
                    Some(&region),
                );
            }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            }
            let bgra: [u8; 4] = unsafe { *(mapped.pData as *const [u8; 4]) };
            let color = (bgra[2], bgra[1], bgra[0]);
            unsafe { self.context.Unmap(&self.staging, 0) };
            Ok(Some(Presented {
                qpc: info.LastPresentTime,
                color,
            }))
        })();
        unsafe { self.duplication.ReleaseFrame()? };
        result
    }
}

/// A visible top-level window of `pid` with this title.
pub struct Window {
    pub hwnd: HWND,
    pub rect: RECT,
}

struct Search {
    pid: u32,
    title: &'static str,
    found: Option<HWND>,
}

unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
    let search = unsafe { &mut *(param.0 as *mut Search) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == search.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let mut title = [0u16; 256];
        let length = unsafe { GetWindowTextW(hwnd, &mut title) } as usize;
        if String::from_utf16_lossy(&title[..length]) == search.title {
            search.found = Some(hwnd);
            return BOOL(0);
        }
    }
    BOOL(1)
}

pub fn find_window(pid: u32, title: &'static str) -> Option<Window> {
    let mut search = Search {
        pid,
        title,
        found: None,
    };
    // EnumWindows reports an error when the callback stops it early.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    let hwnd = search.found?;
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(Window { hwnd, rect })
}

/// Puts `window` above the others without taking focus, so nothing hides the
/// pixel that is watched.
pub fn keep_on_top(window: &Window) {
    let _ = unsafe {
        SetWindowPos(
            window.hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}
