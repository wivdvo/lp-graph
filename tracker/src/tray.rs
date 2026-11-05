//! Tray icon for lp-tracker: shows status, opens the graph, toggles autostart, exits.
//! Runs the Win32 message loop on the main thread; tracking happens on a worker thread.

use common::wide;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_REFRESH: u32 = WM_APP + 2;
const ID_OPEN: usize = 1;
const ID_AUTOSTART: usize = 2;
const ID_EXIT: usize = 3;
const ID_SYNC: usize = 4;

static STATUS: Mutex<String> = Mutex::new(String::new());
static LAST: Mutex<String> = Mutex::new(String::new());
static HWND_TRAY: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);
static ICON: AtomicIsize = AtomicIsize::new(0);

pub struct Hooks {
    pub autostart_on: fn() -> bool,
    pub set_autostart: fn(bool),
    pub sync_on: fn() -> bool,
    pub set_sync: fn(bool),
}
static mut HOOKS: Option<Hooks> = None;

fn refresh() {
    let h = HWND_TRAY.load(Ordering::Relaxed);
    if h != 0 {
        unsafe { PostMessageW(h as HWND, WM_REFRESH, 0, 0) };
    }
}

/// Status line, e.g. "Connected to League client".
pub fn set_status(s: &str) {
    *STATUS.lock().unwrap() = s.to_string();
    refresh();
}

/// Last recorded change, e.g. "Last game: +21 LP → Gold II 37 LP".
pub fn set_last(s: &str) {
    *LAST.lock().unwrap() = s.to_string();
    refresh();
}

fn tooltip() -> String {
    let mut t = format!("LP Tracker – {}", STATUS.lock().unwrap());
    let last = LAST.lock().unwrap();
    if !last.is_empty() {
        t.push('\n');
        t.push_str(&last);
    }
    t.chars().take(120).collect()
}

fn notify(hwnd: HWND, msg: u32) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = ICON.load(Ordering::Relaxed) as HICON;
        let tip: Vec<u16> = tooltip().encode_utf16().collect();
        let n = tip.len().min(nid.szTip.len() - 1);
        nid.szTip[..n].copy_from_slice(&tip[..n]);
        Shell_NotifyIconW(msg, &nid);
    }
}

fn open_graph() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe.with_file_name("lp-graph.exe")).spawn();
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let hooks = (*std::ptr::addr_of!(HOOKS)).as_ref().unwrap();
        let m = CreatePopupMenu();
        let status = STATUS.lock().unwrap().clone();
        let last = LAST.lock().unwrap().clone();
        AppendMenuW(m, MF_STRING | MF_GRAYED, 0, wide(&status).as_ptr());
        if !last.is_empty() {
            AppendMenuW(m, MF_STRING | MF_GRAYED, 0, wide(&last).as_ptr());
        }
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        AppendMenuW(m, MF_STRING, ID_OPEN, wide("Open LP Graph").as_ptr());
        let flags = if (hooks.autostart_on)() { MF_STRING | MF_CHECKED } else { MF_STRING };
        AppendMenuW(m, flags, ID_AUTOSTART, wide("Start with Windows").as_ptr());
        let flags = if (hooks.sync_on)() { MF_STRING | MF_CHECKED } else { MF_STRING };
        AppendMenuW(m, flags, ID_SYNC, wide("Sync to GitHub after each game").as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        AppendMenuW(m, MF_STRING, ID_EXIT, wide("Exit tracker").as_ptr());
        SetMenuDefaultItem(m, ID_OPEN as u32, 0);

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, null());
        DestroyMenu(m);
        match cmd as usize {
            ID_OPEN => open_graph(),
            ID_AUTOSTART => (hooks.set_autostart)(!(hooks.autostart_on)()),
            ID_SYNC => (hooks.set_sync)(!(hooks.sync_on)()),
            ID_EXIT => {
                notify(hwnd, NIM_DELETE);
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lp as u32 {
                WM_LBUTTONUP => open_graph(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_REFRESH => {
            notify(hwnd, NIM_MODIFY);
            0
        }
        WM_DESTROY => {
            notify(hwnd, NIM_DELETE);
            PostQuitMessage(0);
            0
        }
        _ => {
            // Explorer restarted: put the icon back.
            if msg != 0 && msg as isize == TASKBAR_CREATED.load(Ordering::Relaxed) {
                notify(hwnd, NIM_ADD);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

/// Creates the tray icon and runs the message loop. Never returns.
pub fn run(hooks: Hooks) -> ! {
    unsafe {
        HOOKS = Some(hooks);
        if STATUS.lock().unwrap().is_empty() {
            *STATUS.lock().unwrap() = "Waiting for League client".into();
        }
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) as isize, Ordering::Relaxed);
        ICON.store(common::icon::make(GetSystemMetrics(SM_CXSMICON).max(16)) as isize, Ordering::Relaxed);
        let hinst = GetModuleHandleW(null());
        let class = wide("LpTrackerTray");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        // Hidden top-level window (not message-only, so it receives "TaskbarCreated").
        let hwnd = CreateWindowExW(0, class.as_ptr(), wide("LP Tracker").as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), hinst, null());
        HWND_TRAY.store(hwnd as isize, Ordering::Relaxed);
        notify(hwnd, NIM_ADD);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        std::process::exit(0);
    }
}
