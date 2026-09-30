// Win32 setup shared by both ends.

use crate::edge::Rect;
use crate::layout::Layout;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use windows::core::BOOL;

/// Without this, scaled displays report virtualized (wrong) coordinates.
pub fn dpi_aware() {
    if let Err(e) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        eprintln!("warning: could not set per-monitor DPI awareness: {e}");
    }
}

/// The whole virtual desktop (all monitors) as one rectangle. Call after `dpi_aware`.
pub fn virtual_screen() -> Rect {
    unsafe {
        Rect {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// Every monitor's rectangle, and which one is primary. Call after `dpi_aware`.
/// Never call this from a hook callback.
pub fn layout() -> Layout {
    unsafe extern "system" fn each(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let found = unsafe { &mut *(data.0 as *mut Vec<(Rect, bool)>) };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(mon, &mut info) }.as_bool() {
            let r = info.rcMonitor;
            let rect = Rect { left: r.left, top: r.top, w: r.right - r.left, h: r.bottom - r.top };
            found.push((rect, info.dwFlags & MONITORINFOF_PRIMARY != 0));
        }
        true.into() // keep enumerating
    }

    let mut found: Vec<(Rect, bool)> = Vec::new();
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut found as *mut _ as isize)) };
    if found.is_empty() {
        return Layout::single(virtual_screen());
    }
    let primary = found.iter().position(|m| m.1).unwrap_or(0);
    Layout { monitors: found.into_iter().map(|m| m.0).collect(), primary }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: enumerates this machine's monitors. Holds on any machine.
    #[test]
    fn real_layout_agrees_with_windows() {
        dpi_aware();
        let l = layout();
        eprintln!("monitors: {:?}, primary {}", l.monitors, l.primary);
        assert!(!l.monitors.is_empty());
        assert!(l.primary < l.monitors.len());
        assert!(l.monitors.iter().all(|m| m.w > 0 && m.h > 0));
        assert_eq!(l.bounds(), virtual_screen(), "monitor union must equal the virtual screen");
    }
}
