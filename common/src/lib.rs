//! Shared code for lp-tracker and lp-graph: finding the League client,
//! talking to its local API (LCU) over WinHTTP, rank math and the record file.

pub mod http;
pub mod icon;
pub mod rank;
pub mod store;

use std::ffi::c_void;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub const CLIENT_EXE: &str = "LeagueClientUx.exe";
pub const TRACKER_MUTEX: &str = "Local\\LpTracker";

/// Null-terminated UTF-16 string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Returns the pid of the first process with this exe name (case-insensitive).
pub fn find_process(exe: &str) -> Option<u32> {
    let target: Vec<u16> = exe.to_ascii_lowercase().encode_utf16().collect();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name: Vec<u16> = e.szExeFile[..len]
                .iter()
                .map(|&c| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c })
                .collect();
            if name == target {
                found = Some(e.th32ProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        found
    }
}

fn process_dir(pid: u32) -> Option<PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let p = PathBuf::from(String::from_utf16_lossy(&buf[..size as usize]));
        p.parent().map(|d| d.to_path_buf())
    }
}

/// Connection details for the local League client API.
#[derive(Clone)]
pub struct Lcu {
    pub port: u16,
    /// Full "Authorization: Basic ..." header line.
    pub auth_header: String,
}

impl Lcu {
    /// Finds the running client and reads its lockfile (`name:pid:port:password:protocol`).
    pub fn find() -> Option<Lcu> {
        let pid = find_process(CLIENT_EXE)?;
        let mut candidates = Vec::new();
        if let Some(d) = process_dir(pid) {
            candidates.push(d.join("lockfile"));
        }
        candidates.push(PathBuf::from("C:\\Riot Games\\League of Legends\\lockfile"));
        for p in candidates {
            if let Ok(s) = std::fs::read_to_string(&p) {
                let parts: Vec<&str> = s.trim().split(':').collect();
                if parts.len() >= 4 {
                    if let Ok(port) = parts[2].parse::<u16>() {
                        let token = base64(format!("riot:{}", parts[3]).as_bytes());
                        return Some(Lcu { port, auth_header: format!("Authorization: Basic {token}") });
                    }
                }
            }
        }
        None
    }

    /// GET a JSON endpoint. Returns None on any error or non-2xx status.
    pub fn get(&self, path: &str) -> Option<serde_json::Value> {
        let body = http::get(self.port, path, &self.auth_header)?;
        serde_json::from_slice(&body).ok()
    }

    /// Ranked splits of the last two years, oldest first. The client lists each split as its
    /// own "season" entry with metadata.currentSplit = 1, 2, 3…
    pub fn splits(&self) -> Option<Vec<Split>> {
        let body = http::post_json(
            self.port,
            "/lol-seasons/v1/allSeasons/product/LOL",
            &self.auth_header,
            r#"{"lastNYears":2}"#,
        )?;
        let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
        let mut splits: Vec<Split> = v
            .as_array()?
            .iter()
            .filter_map(|s| {
                Some(Split {
                    start: s["seasonStart"].as_i64()?,
                    end: s["seasonEnd"].as_i64().unwrap_or(0),
                    number: s["metadata"]["currentSplit"].as_i64().unwrap_or(0),
                })
            })
            .collect();
        splits.sort_by_key(|s| s.start);
        Some(splits)
    }

    pub fn ranked_stats(&self) -> Option<serde_json::Value> {
        self.get("/lol-ranked/v1/current-ranked-stats")
    }

    /// Match history of the logged-in player, newest first.
    pub fn match_history(&self, beg: usize, end: usize) -> Vec<Game> {
        let path = format!(
            "/lol-match-history/v1/products/lol/current-summoner/matches?begIndex={beg}&endIndex={end}"
        );
        let Some(v) = self.get(&path) else { return Vec::new() };
        let Some(arr) = v["games"]["games"].as_array() else { return Vec::new() };
        arr.iter().filter_map(Game::from_json).collect()
    }
}

/// One game from match history (only what we need).
#[derive(Clone, Debug)]
pub struct Game {
    pub id: i64,
    pub t: i64,
    pub queue_id: i64,
    pub champ: i64,
    pub win: bool,
    pub duration: i64,
}

impl Game {
    pub fn from_json(g: &serde_json::Value) -> Option<Game> {
        let p = g["participants"].get(0)?;
        Some(Game {
            id: g["gameId"].as_i64()?,
            t: g["gameCreation"].as_i64().unwrap_or(0),
            queue_id: g["queueId"].as_i64().unwrap_or(0),
            champ: p["championId"].as_i64().unwrap_or(0),
            win: p["stats"]["win"].as_bool().unwrap_or(false),
            duration: g["gameDuration"].as_i64().unwrap_or(0),
        })
    }
    /// Remakes / aborted games don't change LP.
    pub fn counts(&self) -> bool {
        self.duration >= 330
    }
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// One ranked split (unix ms). `number` is 1, 2 or 3 within its season.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Split {
    pub start: i64,
    pub end: i64,
    pub number: i64,
}

/// The split that contains `t`, if known.
pub fn split_at(splits: &[Split], t: i64) -> Option<Split> {
    splits.iter().rev().find(|s| s.start <= t).copied()
}

/// Start of the season (its split 1) that contains `t`.
pub fn season_start_at(splits: &[Split], t: i64) -> Option<i64> {
    let cur = split_at(splits, t)?;
    splits.iter().rev().find(|s| s.start <= cur.start && s.number == 1).map(|s| s.start).or(Some(cur.start))
}

pub const GRAPH_WINDOW_CLASS: &str = "LpGraphWindow";

/// Brings an already open graph window to the front. Returns false if none is open.
pub fn focus_graph_window() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE};
    unsafe {
        let hwnd = FindWindowW(wide(GRAPH_WINDOW_CLASS).as_ptr(), std::ptr::null());
        if hwnd.is_null() {
            return false;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
        true
    }
}

/// Opaque handle wrapper so raw pointers can be passed to threads.
#[derive(Clone, Copy)]
pub struct SendPtr(pub *mut c_void);
unsafe impl Send for SendPtr {}
