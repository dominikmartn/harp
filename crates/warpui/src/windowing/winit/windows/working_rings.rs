//! Native "working" rings for the framed sidebar (see `Window::set_working_rings`).
//!
//! Warp repaints the whole window for every frame, so a ring animated by the app would repaint
//! the window continuously. Like the macOS layers, these rings live beside the app's rendering:
//! one see-through child window per ring sits above the window's swapchain and turns itself on a
//! timer, drawing a 16 px arc in software. The app only talks to this module when the list of
//! rings changes.

use std::f32::consts::TAU;
use std::sync::{LazyLock, Once};
use std::time::Instant;

use pathfinder_geometry::rect::RectF;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetParent, GetWindowLongPtrW,
    HTTRANSPARENT, HWND_TOP, IsIconic, RegisterClassW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ULW_ALPHA, UpdateLayeredWindow, WM_NCDESTROY, WM_NCHITTEST,
    WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT, WS_VISIBLE,
};
use windows::core::w;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// One turn takes a second, clockwise, like the macOS layer's animation.
const TURN_SECONDS: f32 = 1.0;
/// About 60 frames a second; Windows rounds timers to its own tick.
const FRAME_MILLIS: u32 = 16;
const TIMER_ID: usize = 1;

/// The arc's geometry in a 16 px box (working-arc.svg): radius 6.2, stroke 1.8, a quarter
/// turn left open, from twelve o'clock clockwise to nine.
const ARC_BOX: f32 = 16.;
const ARC_RADIUS: f32 = 6.2;
const ARC_STROKE: f32 = 1.8;
const ARC_SWEEP: f32 = TAU * 0.75;

/// Ink of the sidebar's strong icons, light and dark.
const INK_LIGHT: [u8; 3] = [0x3a, 0x3a, 0x3c];
const INK_DARK: [u8; 3] = [0xe5, 0xe5, 0xea];

/// Every ring reads its phase from here, so rings that appear at different times still turn
/// together.
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// The rings of one window: child windows of `parent`, in the order the app listed them.
pub(in crate::windowing::winit) struct WorkingRings {
    parent: HWND,
    rings: Vec<HWND>,
}

impl WorkingRings {
    pub(in crate::windowing::winit) fn for_window(window: &winit::window::Window) -> Option<Self> {
        let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
            log::warn!("working rings: the window has no Win32 handle");
            return None;
        };
        Some(Self {
            parent: HWND(handle.hwnd.get() as _),
            rings: Vec::new(),
        })
    }

    /// Lays the rings out again: `(icon rect, visible part)` pairs in window points from the top
    /// left. An empty list removes every ring.
    pub(in crate::windowing::winit) fn set(
        &mut self,
        rings: &[(RectF, RectF)],
        scale_factor: f32,
        dark: bool,
    ) {
        for (index, (icon, visible)) in rings.iter().enumerate() {
            if index == self.rings.len() {
                match create_ring_window(self.parent) {
                    Some(hwnd) => self.rings.push(hwnd),
                    None => break,
                }
            }
            place(self.rings[index], *icon, *visible, scale_factor, dark);
        }
        for hwnd in self.rings.drain(rings.len().min(self.rings.len())..) {
            // SAFETY: `hwnd` is a window this struct created and has not destroyed.
            unsafe { DestroyWindow(hwnd).ok() };
        }
    }
}

impl Drop for WorkingRings {
    fn drop(&mut self) {
        self.set(&[], 1., false);
    }
}

/// What one ring window knows; owned by the window through `GWLP_USERDATA`.
struct Ring {
    dc: HDC,
    bitmap: HBITMAP,
    /// Premultiplied BGRA, `size` x `size`, top row first.
    pixels: *mut u32,
    size: usize,
    visible: PixelRect,
    ink: [u8; 3],
}

impl Ring {
    fn new(size: usize) -> Option<Self> {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative: the rows run top to bottom.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        // SAFETY: `info` describes a 32 bit top-down bitmap, and `pixels` receives the address
        // of its memory, which lives as long as the bitmap.
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0).ok()?;
            SelectObject(dc, HGDIOBJ(bitmap.0));
            Some(Self {
                dc,
                bitmap,
                pixels: pixels.cast(),
                size,
                visible: PixelRect::full(size),
                ink: INK_LIGHT,
            })
        }
    }

    /// Draws the arc at the current phase and hands it to the window.
    fn draw(&self, hwnd: HWND) {
        let turned = EPOCH.elapsed().as_secs_f32() / TURN_SECONDS * TAU;
        // SAFETY: `pixels` points at `size * size` pixels owned by the bitmap, and nothing else
        // touches them: drawing happens on the thread that owns the window.
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.pixels, self.size * self.size) };
        rasterize(pixels, self.size, turned, self.visible, self.ink);
        let extent = SIZE {
            cx: self.size as i32,
            cy: self.size as i32,
        };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        // SAFETY: all the pointers outlive the call, and `dc` holds the bitmap drawn above.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                None,
                Some(&extent),
                Some(self.dc),
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .ok();
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `Ring::new` and are released once.
        unsafe {
            DeleteDC(self.dc).ok().ok();
            DeleteObject(HGDIOBJ(self.bitmap.0)).ok().ok();
        }
    }
}

/// A rectangle in whole pixels, `right` and `bottom` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelRect {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

impl PixelRect {
    fn full(size: usize) -> Self {
        Self {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        }
    }

    /// `visible` relative to `icon`, in pixels, clamped to the icon's `size` square.
    fn within(icon: RectF, visible: RectF, scale_factor: f32, size: usize) -> Self {
        let edge = |value: f32| ((value * scale_factor).round().max(0.) as usize).min(size);
        let origin = visible.origin() - icon.origin();
        Self {
            left: edge(origin.x()),
            top: edge(origin.y()),
            right: edge(origin.x() + visible.width()),
            bottom: edge(origin.y() + visible.height()),
        }
    }

    fn contains(&self, x: usize, y: usize) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }
}

/// Fills a `size` square with the arc turned `turned` radians clockwise, anti-aliased by exact
/// coverage and premultiplied. Pixels outside `visible` stay clear.
fn rasterize(
    pixels: &mut [u32],
    size: usize,
    turned: f32,
    visible: PixelRect,
    ink: [u8; 3],
) {
    let scale = size as f32 / ARC_BOX;
    let (radius, half_stroke) = (ARC_RADIUS * scale, ARC_STROKE * scale / 2.);
    let center = size as f32 / 2.;
    // Angles run clockwise from twelve o'clock.
    let point_at = |angle: f32| {
        (
            center + radius * angle.sin(),
            center - radius * angle.cos(),
        )
    };
    let caps = [point_at(turned), point_at(turned + ARC_SWEEP)];
    for y in 0..size {
        for x in 0..size {
            let pixel = &mut pixels[y * size + x];
            *pixel = 0;
            if !visible.contains(x, y) {
                continue;
            }
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            let angle = (dx.atan2(-dy) - turned).rem_euclid(TAU);
            // On the arc the stroke is the distance to its circle; in the gap the nearest
            // stroke is a round cap.
            let distance = if angle <= ARC_SWEEP {
                (dx.hypot(dy) - radius).abs()
            } else {
                caps.iter()
                    .map(|(cx, cy)| (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy))
                    .fold(f32::INFINITY, f32::min)
            };
            let coverage = (half_stroke - distance + 0.5).clamp(0., 1.);
            if coverage > 0. {
                let channel = |value: u8| (value as f32 * coverage).round() as u32;
                let alpha = (coverage * 255.).round() as u32;
                *pixel = alpha << 24 | channel(ink[0]) << 16 | channel(ink[1]) << 8 | channel(ink[2]);
            }
        }
    }
}

/// Moves a ring window onto its icon, clips it to the visible part and restarts its drawing.
fn place(hwnd: HWND, icon: RectF, visible: RectF, scale_factor: f32, dark: bool) {
    let size = (icon.width().max(icon.height()) * scale_factor).round().max(1.) as usize;
    // SAFETY: the user data is the `Ring` boxed in `create_ring_window`, freed only when the
    // window is destroyed, and both run on the window's thread.
    let ring = unsafe {
        let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Ring;
        if data.is_null() {
            return;
        }
        if (*data).size != size {
            // The scale factor changed: swap in a bitmap of the new size.
            let Some(resized) = Ring::new(size) else {
                return;
            };
            *data = resized;
        }
        &mut *data
    };
    ring.visible = PixelRect::within(icon, visible, scale_factor, size);
    ring.ink = if dark { INK_DARK } else { INK_LIGHT };
    // SAFETY: `hwnd` is a live child window.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            (icon.origin().x() * scale_factor).round() as i32,
            (icon.origin().y() * scale_factor).round() as i32,
            size as i32,
            size as i32,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
        .ok();
    }
    ring.draw(hwnd);
}

fn create_ring_window(parent: HWND) -> Option<HWND> {
    static REGISTER: Once = Once::new();
    // SAFETY: plain Win32 window-class and window creation on the UI thread; the class name is a
    // static string, and the boxed `Ring` handed to the window is reclaimed in `WM_NCDESTROY`.
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).ok()?.0);
        REGISTER.call_once(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: w!("WarpWorkingRing"),
                ..Default::default()
            };
            RegisterClassW(&class);
        });
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            w!("WarpWorkingRing"),
            None,
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            Some(instance),
            None,
        )
        .inspect_err(|err| log::warn!("working rings: cannot create a ring window: {err}"))
        .ok()?;
        let Some(ring) = Ring::new(1) else {
            DestroyWindow(hwnd).ok();
            return None;
        };
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(ring)) as isize);
        SetTimer(Some(hwnd), TIMER_ID, FRAME_MILLIS, None);
        Some(hwnd)
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the user data is null or the `Ring` boxed in `create_ring_window`.
    let ring = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Ring).as_ref() };
    match message {
        // Clicks, drags and scrolls reach the app exactly as before.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_TIMER => {
            // SAFETY: `hwnd` is a live window, so its parent handle is valid or null.
            let minimized = unsafe { IsIconic(GetParent(hwnd).unwrap_or_default()).as_bool() };
            if let (Some(ring), false) = (ring, minimized) {
                ring.draw(hwnd);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: the window is gone; the box it owned is reclaimed exactly once.
            unsafe {
                let data = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Ring;
                if !data.is_null() {
                    drop(Box::from_raw(data));
                }
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        // SAFETY: forwarding the unhandled message with its own arguments.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
#[path = "working_rings_tests.rs"]
mod tests;
