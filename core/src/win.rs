// Win32 setup shared by both ends.

use crate::edge::Rect;
use crate::layout::Layout;
use std::io;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, RECT, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};
use windows::core::{BOOL, HSTRING};

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

/// The first copy of a program in this user session, from `single_instance`.
pub struct Instance(HANDLE);

// The event handle is a kernel object, usable from any thread.
unsafe impl Send for Instance {}

impl Instance {
    /// Block until a later launch asks this copy to come forward. False if
    /// waiting failed (then stop waiting).
    pub fn wait(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, INFINITE) == WAIT_OBJECT_0 }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// One running copy per user session, through a named event (name it
/// `Local\...`). The first caller gets `Some`. A later caller signals the
/// first one, lets it take the foreground (Windows refuses a background
/// process otherwise), and gets `None`: it should exit.
pub fn single_instance(name: &str) -> io::Result<Option<Instance>> {
    let event = unsafe { CreateEventW(None, false, false, &HSTRING::from(name)) }.map_err(io::Error::other)?;
    // Read at once: CreateEventW succeeds either way and says which in the last error.
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Ok(Some(Instance(event)));
    }
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let _ = SetEvent(event);
        let _ = CloseHandle(event);
    }
    Ok(None)
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

    #[test]
    fn a_second_copy_wakes_the_first_and_is_told_to_exit() {
        let name = format!("Local\\input-share-test-{}", std::process::id());
        let first = single_instance(&name).unwrap().expect("the first copy runs");
        assert!(single_instance(&name).unwrap().is_none(), "a second copy must exit");
        assert!(first.wait(), "the second copy signals the first");
        drop(first);
        assert!(single_instance(&name).unwrap().is_some(), "after the first quits, a new copy runs");
    }
}
