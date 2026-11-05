//! LP-Graph-Setup.exe: one-file installer and uninstaller for LP Graph.
//!
//! Install: copies lp-tracker.exe and lp-graph.exe (embedded in this file) to
//! %LOCALAPPDATA%\Programs\LP Graph (no admin rights needed), adds a Start Menu shortcut and
//! an entry in Windows' "Installed apps", then starts the tracker and opens the graph.
//! Running it again updates an existing install and keeps your data.
//!
//! Options: --uninstall, --quiet (no dialogs), --no-start (don't launch the programs),
//! --dir <folder> (install somewhere else).
#![windows_subsystem = "windows"]

use common::wide;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const TRACKER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lp-tracker.exe"));
const GRAPH: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lp-graph.exe"));
const UNINSTALL_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\LPGraph";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const TITLE: &str = "LP Graph Setup";

struct Opts {
    quiet: bool,
    start: bool,
    dir: Option<PathBuf>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = Opts {
        quiet: args.iter().any(|a| a == "--quiet"),
        start: !args.iter().any(|a| a == "--no-start"),
        dir: args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).map(PathBuf::from),
    };
    let result = if args.iter().any(|a| a == "--uninstall") { uninstall(&opts) } else { install(&opts) };
    if let Err(e) = result {
        message(&opts, &format!("Something went wrong:\n\n{e}"), MB_ICONERROR);
        std::process::exit(1);
    }
}

fn default_dir() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\"));
    local.join("Programs").join("LP Graph")
}

fn start_menu_shortcut() -> PathBuf {
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
    roaming.join("Microsoft\\Windows\\Start Menu\\Programs\\LP Graph.lnk")
}

// ---------- install ----------

fn install(o: &Opts) -> Result<(), String> {
    let dir = o.dir.clone().unwrap_or_else(default_dir);
    let update = dir.join("lp-tracker.exe").exists();
    let question = if update {
        format!("Update LP Graph to version {}?\n\n{}\n\nYour LP history is kept.", env!("CARGO_PKG_VERSION"), dir.display())
    } else {
        format!(
            "Install LP Graph {} to:\n\n{}\n\nLP Tracker will start with Windows and record your LP after every ranked game.",
            env!("CARGO_PKG_VERSION"),
            dir.display()
        )
    };
    if !confirm(o, &question) {
        return Ok(());
    }

    // running copies from this folder must close before their files can be replaced
    stop_programs_in(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    write(&dir.join("lp-tracker.exe"), TRACKER)?;
    write(&dir.join("lp-graph.exe"), GRAPH)?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstaller = dir.join("uninstall.exe");
    if me != uninstaller {
        std::fs::copy(&me, &uninstaller).map_err(|e| format!("Couldn't write {}: {e}", uninstaller.display()))?;
    }

    let graph = dir.join("lp-graph.exe");
    if let Err(e) = shortcut::create(&start_menu_shortcut(), &graph, &dir, "Your League of Legends LP, game by game") {
        // not fatal: the programs work without it
        message(o, &format!("The Start Menu shortcut couldn't be created ({e}).\nYou can start LP Graph from:\n{}", graph.display()), MB_ICONWARNING);
    }
    register_uninstall(&dir);

    if o.start {
        // the tracker adds itself to Windows startup when it runs
        let _ = Command::new(dir.join("lp-tracker.exe")).current_dir(&dir).spawn();
        let _ = Command::new(&graph).current_dir(&dir).spawn();
    }
    let done = if update {
        "LP Graph is updated.".to_string()
    } else {
        "LP Graph is installed.\n\nLP Tracker now runs in the tray (you may need to click ^ to see it) and starts with Windows. Open the graph from the Start Menu or by clicking the tray icon.".to_string()
    };
    message(o, &done, MB_ICONINFORMATION);
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

fn register_uninstall(dir: &Path) {
    let set = |name: &str, value: &str| unsafe {
        let v = wide(value);
        RegSetKeyValueW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr(), wide(name).as_ptr(), REG_SZ, v.as_ptr() as *const _, (v.len() * 2) as u32);
    };
    let set_dword = |name: &str, value: u32| unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr(), wide(name).as_ptr(), REG_DWORD, &value as *const u32 as *const _, 4);
    };
    set("DisplayName", "LP Graph");
    set("DisplayVersion", env!("CARGO_PKG_VERSION"));
    set("Publisher", "github.com/wivdvo/lp-graph");
    set("URLInfoAbout", "https://github.com/wivdvo/lp-graph");
    set("DisplayIcon", &dir.join("lp-graph.exe").display().to_string());
    set("InstallLocation", &dir.display().to_string());
    set("UninstallString", &format!("\"{}\" --uninstall", dir.join("uninstall.exe").display()));
    set("QuietUninstallString", &format!("\"{}\" --uninstall --quiet", dir.join("uninstall.exe").display()));
    set_dword("NoModify", 1);
    set_dword("NoRepair", 1);
    set_dword("EstimatedSize", ((TRACKER.len() + GRAPH.len()) as u32 * 2) / 1024);
}

// ---------- uninstall ----------

fn uninstall(o: &Opts) -> Result<(), String> {
    // uninstall.exe lives in the install folder
    let dir = o.dir.clone().unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_else(default_dir)
    });
    if !confirm(o, "Uninstall LP Graph?\n\nLP Tracker will stop and no longer start with Windows.") {
        return Ok(());
    }
    stop_programs_in(&dir);
    remove_autostart_if_in(&dir);
    let _ = std::fs::remove_file(start_menu_shortcut());
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(UNINSTALL_KEY).as_ptr()) };
    for f in ["lp-tracker.exe", "lp-graph.exe"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let data = dir.join("data");
    if data.exists() {
        let delete = !o.quiet
            && ask(&format!(
                "Also delete your LP history?\n\n{}\n\nChoose No to keep it (for example to reinstall later).",
                data.display()
            ));
        if delete {
            let _ = std::fs::remove_dir_all(&data);
        }
    }
    // this program is running from the folder, so remove it (and the folder, if empty) once it has exited
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if me.parent() == Some(dir.as_path()) {
        let cmd = format!(
            "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rmdir \"{}\"",
            me.display(),
            dir.display()
        );
        use std::os::windows::process::CommandExt;
        // raw_arg: cmd doesn't understand Rust's \" quote escaping; run from outside the
        // folder so it can be removed
        let outside = dir.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
        let _ = Command::new("cmd")
            .raw_arg(format!("/c {cmd}"))
            .current_dir(outside)
            .creation_flags(0x0800_0000)
            .spawn();
    } else {
        let _ = std::fs::remove_file(dir.join("uninstall.exe"));
        let _ = std::fs::remove_dir(&dir);
    }
    message(o, "LP Graph is uninstalled.", MB_ICONINFORMATION);
    Ok(())
}

/// Removes the tracker's Windows startup entry, but only if it points into `dir`.
fn remove_autostart_if_in(dir: &Path) {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let ok = RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide("LpTracker").as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut _,
            &mut size,
        ) == 0;
        if !ok {
            return;
        }
        let value = String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]).to_lowercase();
        if value.contains(&dir.display().to_string().to_lowercase()) {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide("LpTracker").as_ptr());
        }
    }
}

// ---------- stopping running programs ----------

/// Closes lp-tracker.exe / lp-graph.exe processes that run from `dir`: politely first (so
/// the tray icon disappears), then forcefully after 3 seconds.
fn stop_programs_in(dir: &Path) {
    let prefix = dir.display().to_string().to_lowercase();
    for pid in processes_in(&prefix) {
        unsafe {
            EnumWindows(Some(close_windows_of), pid as LPARAM);
            let h = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                if WaitForSingleObject(h, 3000) != 0 {
                    TerminateProcess(h, 0);
                    WaitForSingleObject(h, 2000);
                }
                CloseHandle(h);
            }
        }
    }
}

unsafe extern "system" fn close_windows_of(hwnd: HWND, pid: LPARAM) -> BOOL {
    let mut owner = 0u32;
    GetWindowThreadProcessId(hwnd, &mut owner);
    if owner as LPARAM == pid {
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
    1
}

fn processes_in(prefix: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
            if name == "lp-tracker.exe" || name == "lp-graph.exe" {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.th32ProcessID);
                if !h.is_null() {
                    let mut buf = [0u16; 1024];
                    let mut size = buf.len() as u32;
                    if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
                        let path = String::from_utf16_lossy(&buf[..size as usize]).to_lowercase();
                        if path.starts_with(prefix) {
                            pids.push(e.th32ProcessID);
                        }
                    }
                    CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    pids
}

// ---------- dialogs ----------

fn message(o: &Opts, text: &str, icon: MESSAGEBOX_STYLE) {
    if !o.quiet {
        unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide(TITLE).as_ptr(), MB_OK | icon) };
    }
}

fn confirm(o: &Opts, text: &str) -> bool {
    o.quiet || ask(text)
}

fn ask(text: &str) -> bool {
    unsafe { MessageBoxW(null_mut(), wide(text).as_ptr(), wide(TITLE).as_ptr(), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

// ---------- Start Menu shortcut ----------

mod shortcut {
    use std::path::Path;
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    pub fn create(lnk: &Path, target: &Path, workdir: &Path, description: &str) -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            link.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| e.to_string())?;
            link.SetWorkingDirectory(&HSTRING::from(workdir.as_os_str())).map_err(|e| e.to_string())?;
            link.SetDescription(&HSTRING::from(description)).map_err(|e| e.to_string())?;
            link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(|e| e.to_string())?;
            let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())
        }
    }
}
