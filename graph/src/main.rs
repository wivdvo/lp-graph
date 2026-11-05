//! lp-graph: the window. Reads lp.jsonl (written by lp-tracker) plus a cache of the
//! client's match history, and draws a game-by-game LP graph with raw Win32 + GDI.
#![windows_subsystem = "windows"]

use common::rank::{self, Standing, QUEUES};
use common::store::{self, Kind, Record};
use common::{now_ms, wide, Game, Lcu, SendPtr, TRACKER_MUTEX};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{OpenMutexW, WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_F5};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_RELOAD: u32 = WM_APP + 1;
const WM_STATUS: u32 = WM_APP + 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

// ---------- colors ----------
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}
const BG: u32 = rgb(0x12, 0x14, 0x18);
const PANEL: u32 = rgb(0x1a, 0x1d, 0x23);
const GRID: u32 = rgb(0x24, 0x28, 0x30);
const GRID_TIER: u32 = rgb(0x3b, 0x41, 0x4d);
const TEXT: u32 = rgb(0xe6, 0xe6, 0xe6);
const MUTED: u32 = rgb(0x8b, 0x92, 0x9e);
const LINE: u32 = rgb(0x5b, 0x9b, 0xd5);
const LINE_EST: u32 = rgb(0x34, 0x4c, 0x66);
const WIN: u32 = rgb(0x3f, 0xb9, 0x50);
const LOSS: u32 = rgb(0xf8, 0x51, 0x49);
const EST: u32 = rgb(0x6e, 0x75, 0x80);

fn tier_color(tier: &str) -> u32 {
    match tier {
        "IRON" => rgb(0x8a, 0x7b, 0x73),
        "BRONZE" => rgb(0xb0, 0x7a, 0x52),
        "SILVER" => rgb(0xa8, 0xb2, 0xbd),
        "GOLD" => rgb(0xd4, 0xb2, 0x6a),
        "PLATINUM" => rgb(0x4e, 0xb0, 0xa8),
        "EMERALD" => rgb(0x2b, 0xc2, 0x7a),
        "DIAMOND" => rgb(0x7b, 0x9c, 0xf5),
        "MASTER" | "GRANDMASTER" | "CHALLENGER" => rgb(0xc0, 0x6b, 0xe6),
        _ => TEXT,
    }
}

// ---------- data ----------
#[derive(Clone, Copy, PartialEq)]
enum PtKind {
    Estimated,
    Catchup,
    Exact,
}

#[derive(Clone)]
struct Pt {
    abs: i64,
    delta: i64,
    win: bool,
    kind: PtKind,
    t: i64,
    champ: i64,
}

#[derive(Default)]
struct Cache {
    games: Vec<Game>,
    champs: HashMap<i64, String>,
    live: HashMap<String, Standing>,
    /// Start of the current ranked split (unix ms), from the client. 0 = unknown.
    split_start: i64,
    /// Start of the current ranked season (its first split), unix ms. 0 = unknown.
    season_start: i64,
}

fn cache_path() -> PathBuf {
    store::data_dir().join("history_cache.json")
}

fn load_cache() -> Cache {
    let mut c = Cache::default();
    let Ok(s) = std::fs::read_to_string(cache_path()) else { return c };
    let Ok(v) = serde_json::from_str::<Value>(&s) else { return c };
    if let Some(arr) = v["games"].as_array() {
        for g in arr {
            c.games.push(Game {
                id: g["id"].as_i64().unwrap_or(0),
                t: g["t"].as_i64().unwrap_or(0),
                queue_id: g["q"].as_i64().unwrap_or(0),
                champ: g["c"].as_i64().unwrap_or(0),
                win: g["w"].as_bool().unwrap_or(false),
                duration: g["d"].as_i64().unwrap_or(0),
            });
        }
    }
    if let Some(m) = v["champs"].as_object() {
        for (k, n) in m {
            if let (Ok(id), Some(n)) = (k.parse(), n.as_str()) {
                c.champs.insert(id, n.to_string());
            }
        }
    }
    c.split_start = v["splitStart"].as_i64().unwrap_or(0);
    c.season_start = v["seasonStart"].as_i64().unwrap_or(0);
    if let Some(m) = v["live"].as_object() {
        for (q, s) in m {
            c.live.insert(
                q.clone(),
                Standing {
                    tier: s["tier"].as_str().unwrap_or("").into(),
                    div: s["div"].as_str().unwrap_or("").into(),
                    lp: s["lp"].as_i64().unwrap_or(0),
                    wins: s["w"].as_i64().unwrap_or(0),
                    losses: s["l"].as_i64().unwrap_or(0),
                    abs: s["abs"].as_i64().unwrap_or(0),
                },
            );
        }
    }
    c
}

fn save_cache(c: &Cache) {
    let games: Vec<Value> = c
        .games
        .iter()
        .map(|g| json!({"id": g.id, "t": g.t, "q": g.queue_id, "c": g.champ, "w": g.win, "d": g.duration}))
        .collect();
    let champs: serde_json::Map<String, Value> = c.champs.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let live: serde_json::Map<String, Value> = c
        .live
        .iter()
        .map(|(q, s)| {
            (q.clone(), json!({"tier": s.tier, "div": s.div, "lp": s.lp, "w": s.wins, "l": s.losses, "abs": s.abs}))
        })
        .collect();
    let v = json!({"games": games, "champs": champs, "live": live, "splitStart": c.split_start, "seasonStart": c.season_start});
    let tmp = cache_path().with_extension("tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, cache_path());
    }
}

static STATUS: Mutex<String> = Mutex::new(String::new());
static BACKFILL_RUNNING: Mutex<bool> = Mutex::new(false);

fn set_status(hwnd: SendPtr, s: &str) {
    *STATUS.lock().unwrap() = s.to_string();
    unsafe { PostMessageW(hwnd.0, WM_STATUS, 0, 0) };
}

/// Reads the full match history (+ champion names, current ranks) from the client into the cache.
fn backfill(hwnd: SendPtr) {
    {
        let mut running = BACKFILL_RUNNING.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }
    let done = |msg: &str| {
        set_status(hwnd, msg);
        *BACKFILL_RUNNING.lock().unwrap() = false;
    };
    let Some(lcu) = Lcu::find() else {
        done("League client not running – showing saved history");
        return;
    };
    set_status(hwnd, "Reading match history…");
    let mut cache = load_cache();
    let mut known: HashSet<i64> = cache.games.iter().map(|g| g.id).collect();
    let mut beg = 0;
    let mut total = 0;
    // The client seems to keep the range of its first match-history request for the
    // whole session, so always start with the largest range (200 games).
    while beg < 2000 {
        let size = if beg == 0 { 200 } else { 100 };
        let page = lcu.match_history(beg, beg + size);
        let n = page.len();
        let mut added = 0;
        for g in page {
            if known.insert(g.id) {
                cache.games.push(g);
                added += 1;
            }
        }
        total += n;
        if n < size || (added == 0 && beg > 0) {
            break;
        }
        beg += size;
    }
    if total == 0 && cache.games.is_empty() {
        done("Couldn't read match history (are you logged in?)");
        return;
    }
    cache.games.sort_by_key(|g| g.t);
    if cache.champs.is_empty() {
        if let Some(arr) = lcu.get("/lol-game-data/assets/v1/champion-summary.json") {
            for c in arr.as_array().into_iter().flatten() {
                if let (Some(id), Some(n)) = (c["id"].as_i64(), c["name"].as_str()) {
                    cache.champs.insert(id, n.to_string());
                }
            }
        }
    }
    if let Some((split, season)) = lcu.season_dates() {
        cache.split_start = split;
        cache.season_start = season;
    } else if let Some(season) = lcu.get("/lol-seasons/v1/season/product/LOL") {
        // fallback: this only knows the current split
        if let Some(start) = season["seasonStart"].as_i64() {
            cache.split_start = start;
        }
    }
    if let Some(stats) = lcu.ranked_stats() {
        for (q, _, _) in QUEUES {
            if let Some(s) = rank::standing(&stats, q) {
                cache.live.insert(q.to_string(), s);
            }
        }
    }
    save_cache(&cache);
    unsafe { PostMessageW(hwnd.0, WM_RELOAD, 0, 0) };
    done(&format!("Match history loaded ({} games)", cache.games.len()));
}

// ---------- app state ----------
struct App {
    queue: usize,
    records: Vec<Record>,
    cache: Cache,
    series: [Vec<Pt>; 2],
    hover: Option<usize>,
    tracker_running: bool,
    sync: Option<(String, i64, i64)>,
    scale: f32,
    // hit areas (client px)
    tabs: [RECT; 2],
    refresh_btn: RECT,
    range: usize,
    range_btns: [RECT; 5],
    tracker_btn: RECT,
    plot: RECT,
}

thread_local! {
    static APP: RefCell<App> = RefCell::new(App {
        queue: 0,
        records: Vec::new(),
        cache: Cache::default(),
        series: [Vec::new(), Vec::new()],
        hover: None,
        tracker_running: false,
        sync: None,
        scale: 1.0,
        tabs: [RECT { left: 0, top: 0, right: 0, bottom: 0 }; 2],
        refresh_btn: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        range: 0, // Last 30
        range_btns: [RECT { left: 0, top: 0, right: 0, bottom: 0 }; 5],
        tracker_btn: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        plot: RECT { left: 0, top: 0, right: 0, bottom: 0 },
    });
}

fn build_series(records: &[Record], cache: &Cache, queue: &str, qid: i64) -> Vec<Pt> {
    let recs: Vec<&Record> = records.iter().filter(|r| r.queue == queue).collect();
    let recorded_ids: HashSet<i64> = recs.iter().map(|r| r.game_id).filter(|&i| i != 0).collect();

    // Anchor: LP just before the first thing the tracker recorded (or the live rank).
    let (anchor_t, anchor_abs) = match recs.first() {
        Some(f) => (f.t, f.abs - if f.is_game() { f.delta } else { 0 }),
        None => match cache.live.get(queue) {
            Some(s) => (i64::MAX, s.abs),
            None => return Vec::new(),
        },
    };
    let avg = store::averages(records, queue);
    let older: Vec<&Game> = cache
        .games
        .iter()
        .filter(|g| g.queue_id == qid && g.counts() && g.t < anchor_t && !recorded_ids.contains(&g.id))
        .collect();
    // Walk backwards from the anchor.
    let mut est = Vec::with_capacity(older.len());
    let mut cur = anchor_abs;
    for g in older.iter().rev() {
        let d = if g.win { avg.0.round() as i64 } else { -(avg.1.round() as i64) };
        est.push(Pt { abs: cur.max(0), delta: d, win: g.win, kind: PtKind::Estimated, t: g.t, champ: g.champ });
        cur -= d;
    }
    est.reverse();
    let mut pts = est;
    for r in recs.iter().filter(|r| r.is_game()) {
        // start time stored in the record; older records fall back to the cache, then to the record time
        let t = if r.start > 0 {
            r.start
        } else {
            cache.games.iter().find(|g| g.id == r.game_id && r.game_id != 0).map(|g| g.t).unwrap_or(r.t)
        };
        pts.push(Pt {
            abs: r.abs,
            delta: r.delta,
            win: r.win,
            kind: match r.kind {
                Kind::Exact => PtKind::Exact,
                Kind::Catchup => PtKind::Catchup,
                _ => PtKind::Estimated,
            },
            t,
            champ: r.champ,
        });
    }
    pts
}

fn current_standing(app: &App, queue: &str) -> Option<Standing> {
    let rec = app.records.iter().rev().find(|r| r.queue == queue);
    let live = app.cache.live.get(queue);
    match (rec, live) {
        // prefer whichever is newer: records carry a timestamp, the cache is "as of last refresh"
        (Some(r), Some(l)) if l.wins + l.losses > r.wins + r.losses => Some(l.clone()),
        (Some(r), _) => Some(Standing {
            tier: r.tier.clone(),
            div: r.div.clone(),
            lp: r.lp,
            wins: r.wins,
            losses: r.losses,
            abs: r.abs,
        }),
        (None, Some(l)) => Some(l.clone()),
        _ => None,
    }
}

/// Contents of bin/data/sync-status.json: (state, time ms, unpushed commits).
fn load_sync_status() -> Option<(String, i64, i64)> {
    let s = std::fs::read_to_string(store::data_dir().join("sync-status.json")).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    Some((v["state"].as_str()?.to_string(), v["t"].as_i64().unwrap_or(0), v["pending"].as_i64().unwrap_or(0)))
}

fn ago(t_ms: i64) -> String {
    let m = (now_ms() - t_ms).max(0) / 60_000;
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        60..=1439 => format!("{} h ago", m / 60),
        _ => format!("{} days ago", m / 1440),
    }
}

/// Dot color and text for the footer's GitHub backup indicator.
fn backup_status(s: &Option<(String, i64, i64)>) -> (u32, String) {
    match s {
        Some((st, t, _)) if st == "ok" => (WIN, format!("Backed up to GitHub · {}", ago(*t))),
        Some((st, _, n)) if st == "failed" => {
            let pending = if *n > 0 { format!(" · {n} not pushed") } else { String::new() };
            (LOSS, format!("GitHub backup failed{pending} · retries after next game"))
        }
        Some((st, _, _)) if st == "off" => (EST, "GitHub backup off".into()),
        Some((st, _, _)) if st == "norepo" => (EST, "No git repo – backup off".into()),
        _ => (EST, "GitHub backup: waiting for tracker".into()),
    }
}

fn reload(app: &mut App) {
    app.records = store::load_records();
    app.cache = load_cache();
    app.sync = load_sync_status();
    for (i, (q, qid, _)) in QUEUES.iter().enumerate() {
        app.series[i] = build_series(&app.records, &app.cache, q, *qid);
    }
    app.hover = None;
}

fn tracker_running() -> bool {
    unsafe {
        let h = OpenMutexW(0x0010_0000 /* SYNCHRONIZE */, 0, wide(TRACKER_MUTEX).as_ptr());
        if h.is_null() {
            false
        } else {
            CloseHandle(h);
            true
        }
    }
}

fn start_tracker() {
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.with_file_name("lp-tracker.exe");
        let _ = std::process::Command::new(p).spawn();
    }
}

// ---------- time ----------
fn local_time(t_ms: i64) -> SYSTEMTIME {
    unsafe {
        let ft = (t_ms as u64) * 10_000 + 116_444_736_000_000_000;
        let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
        let mut st: SYSTEMTIME = std::mem::zeroed();
        let mut lt: SYSTEMTIME = std::mem::zeroed();
        FileTimeToSystemTime(&f, &mut st);
        SystemTimeToTzSpecificLocalTime(null(), &st, &mut lt);
        lt
    }
}

fn same_day(a: &SYSTEMTIME, b: &SYSTEMTIME) -> bool {
    a.wYear == b.wYear && a.wMonth == b.wMonth && a.wDay == b.wDay
}

fn fmt_time(t_ms: i64) -> String {
    let t = local_time(t_ms);
    let wd = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.wDayOfWeek as usize % 7];
    format!("{wd} {:02}.{:02}.{} {:02}:{:02}", t.wDay, t.wMonth, t.wYear, t.wHour, t.wMinute)
}

// ---------- GDI helpers ----------
struct Gfx {
    dc: HDC,
    s: f32,
}

impl Gfx {
    fn px(&self, v: f32) -> i32 {
        (v * self.s).round() as i32
    }
    fn fill(&self, r: RECT, c: u32) {
        unsafe {
            let b = CreateSolidBrush(c);
            FillRect(self.dc, &r, b);
            DeleteObject(b);
        }
    }
    fn round_rect(&self, r: RECT, fill: u32, border: u32, radius: i32) {
        unsafe {
            let b = CreateSolidBrush(fill);
            let p = CreatePen(PS_SOLID, 1, border);
            let ob = SelectObject(self.dc, b);
            let op = SelectObject(self.dc, p);
            RoundRect(self.dc, r.left, r.top, r.right, r.bottom, radius, radius);
            SelectObject(self.dc, ob);
            SelectObject(self.dc, op);
            DeleteObject(b);
            DeleteObject(p);
        }
    }
    fn line(&self, x1: i32, y1: i32, x2: i32, y2: i32, c: u32, w: i32, dotted: bool) {
        unsafe {
            let p = CreatePen(if dotted { PS_DOT } else { PS_SOLID }, if dotted { 1 } else { w }, c);
            let op = SelectObject(self.dc, p);
            MoveToEx(self.dc, x1, y1, null_mut());
            LineTo(self.dc, x2, y2);
            SelectObject(self.dc, op);
            DeleteObject(p);
        }
    }
    fn polyline(&self, pts: &[POINT], c: u32, w: i32) {
        if pts.len() < 2 {
            return;
        }
        unsafe {
            let p = CreatePen(PS_SOLID, w, c);
            let op = SelectObject(self.dc, p);
            Polyline(self.dc, pts.as_ptr(), pts.len() as i32);
            SelectObject(self.dc, op);
            DeleteObject(p);
        }
    }
    fn circle(&self, cx: i32, cy: i32, r: i32, fill: Option<u32>, border: u32, bw: i32) {
        unsafe {
            let p = CreatePen(PS_SOLID, bw, border);
            let b = match fill {
                Some(c) => CreateSolidBrush(c),
                None => GetStockObject(NULL_BRUSH) as HBRUSH,
            };
            let op = SelectObject(self.dc, p);
            let ob = SelectObject(self.dc, b);
            Ellipse(self.dc, cx - r, cy - r, cx + r + 1, cy + r + 1);
            SelectObject(self.dc, op);
            SelectObject(self.dc, ob);
            DeleteObject(p);
            if fill.is_some() {
                DeleteObject(b);
            }
        }
    }
    fn font(&self, size: f32, bold: bool) -> HFONT {
        self.font_face(size, bold, "Segoe UI")
    }
    fn font_face(&self, size: f32, bold: bool, face: &str) -> HFONT {
        unsafe {
            CreateFontW(
                -self.px(size),
                0,
                0,
                0,
                if bold { 600 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_DEFAULT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                CLEARTYPE_QUALITY as _,
                (DEFAULT_PITCH | FF_DONTCARE) as _,
                wide(face).as_ptr(),
            )
        }
    }
    fn text_size(&self, s: &str, font: HFONT) -> (i32, i32) {
        unsafe {
            let w: Vec<u16> = s.encode_utf16().collect();
            let of = SelectObject(self.dc, font);
            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(self.dc, w.as_ptr(), w.len() as i32, &mut sz);
            SelectObject(self.dc, of);
            (sz.cx, sz.cy)
        }
    }
    /// align: 0 = left, 1 = center, 2 = right. y is the top of the text.
    fn text(&self, x: i32, y: i32, s: &str, c: u32, font: HFONT, align: u8) -> i32 {
        let (w, _) = self.text_size(s, font);
        let x = match align {
            1 => x - w / 2,
            2 => x - w,
            _ => x,
        };
        unsafe {
            let wv: Vec<u16> = s.encode_utf16().collect();
            let of = SelectObject(self.dc, font);
            SetBkMode(self.dc, TRANSPARENT as _);
            SetTextColor(self.dc, c);
            TextOutW(self.dc, x, y, wv.as_ptr(), wv.len() as i32);
            SelectObject(self.dc, of);
        }
        x + w
    }
}

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT { left: l, top: t, right: r, bottom: b }
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

// ---------- drawing ----------
fn draw(g: &Gfx, w: i32, h: i32, app: &mut App) {
    let s = |v: f32| g.px(v);
    g.fill(rect(0, 0, w, h), BG);

    let f_big = g.font_face(34.0, true, "Palatino Linotype"); // serif with lining numbers (Georgia drops digits below the line)
    let f_mid = g.font(13.0, false);
    let f_mid_b = g.font(13.0, true);
    let f_small = g.font(11.0, false);
    let f_sub = g.font(17.0, false);
    let f_sub_b = g.font(17.0, true);
    let (qname, _, qlabel) = QUEUES[app.queue];
    let all = app.series[app.queue].clone();
    let off = visible_offset(app);
    let pts = all[off..].to_vec();

    // --- header ---
    let cur = current_standing(app, qname);
    let today = local_time(now_ms());
    let today_net: i64 = all
        .iter()
        .filter(|p| p.kind != PtKind::Estimated && same_day(&local_time(p.t), &today))
        .map(|p| p.delta)
        .sum();
    // week starts Monday 00:00 local time
    let monday = day_number(&today) - ((today.wDayOfWeek as i64 + 6) % 7);
    let week_net: i64 = all
        .iter()
        .filter(|p| p.kind != PtKind::Estimated && day_number(&local_time(p.t)) >= monday)
        .map(|p| p.delta)
        .sum();
    let n_est = all.iter().filter(|p| p.kind == PtKind::Estimated).count();
    match &cur {
        Some(c) => {
            let title = rank::label(&c.tier, &c.div, c.lp);
            g.text(s(22.0), s(8.0), &title, tier_color(&c.tier), f_big, 0);
            let games = c.wins + c.losses;
            let wr = if games > 0 { c.wins * 100 / games } else { 0 };
            let gain_color = |v: i64| if v > 0 { WIN } else if v < 0 { LOSS } else { TEXT };
            let y = s(55.0);
            let mut x = g.text(s(23.0), y, &format!("{qlabel}  ·  {}W {}L (", c.wins, c.losses), TEXT, f_sub, 0);
            x = g.text(x, y, &format!("{wr}%"), TEXT, f_sub_b, 0);
            x = g.text(x, y, ")  ·  Today ", TEXT, f_sub, 0);
            x = g.text(x, y, &format!("{today_net:+} LP"), gain_color(today_net), f_sub_b, 0);
            x = g.text(x, y, "  ·  This week ", TEXT, f_sub, 0);
            x = g.text(x, y, &format!("{week_net:+} LP"), gain_color(week_net), f_sub_b, 0);
            g.text(x, y, &format!("  ·  {} games ({n_est} est.)", all.len()), MUTED, f_sub, 0);
        }
        None => {
            g.text(s(22.0), s(8.0), "Unranked", TEXT, f_big, 0);
            g.text(s(23.0), s(55.0), &format!("{qlabel}  ·  no ranked data yet"), TEXT, f_sub, 0);
        }
    }

    // queue tabs + refresh (top right)
    let tw = s(92.0);
    let th = s(28.0);
    let ty = s(18.0);
    let mut x = w - s(20.0) - 2 * tw - s(6.0);
    for (i, (_, _, label)) in QUEUES.iter().enumerate() {
        let r = rect(x, ty, x + tw, ty + th);
        let active = i == app.queue;
        g.round_rect(r, if active { GRID_TIER } else { PANEL }, if active { LINE } else { GRID_TIER }, s(8.0));
        g.text(x + tw / 2, ty + s(6.0), label, if active { TEXT } else { MUTED }, if active { f_mid_b } else { f_mid }, 1);
        app.tabs[i] = r;
        x += tw + s(6.0);
    }
    let rw = s(80.0);
    let rx = w - s(20.0) - 2 * tw - s(6.0) - s(10.0) - rw;
    let rr = rect(rx, ty, rx + rw, ty + th);
    g.round_rect(rr, PANEL, GRID_TIER, s(8.0));
    g.text(rx + rw / 2, ty + s(6.0), "Refresh", MUTED, f_mid, 1);
    app.refresh_btn = rr;

    // range pills (second row, right-aligned): Last 30 / Last 90 / All
    let pw_ = s(68.0);
    let ph_ = s(22.0);
    let py = s(57.0);
    let mut px_ = w - s(20.0) - 5 * pw_ - 4 * s(6.0);
    for (i, label) in RANGE_LABELS.iter().enumerate() {
        let r = rect(px_, py, px_ + pw_, py + ph_);
        let active = i == app.range;
        g.round_rect(r, if active { GRID_TIER } else { PANEL }, if active { LINE } else { GRID_TIER }, s(8.0));
        g.text(px_ + pw_ / 2, py + s(4.0), label, if active { TEXT } else { MUTED }, f_small, 1);
        app.range_btns[i] = r;
        px_ += pw_ + s(6.0);
    }

    // climb rate (third row): last 3 exact wins vs last 3 exact losses
    let cy_ = s(90.0);
    let climb_end = match climb_rate(&all) {
        Some((win_avg, nw, loss_avg, nl)) => {
            let diff = win_avg - loss_avg;
            let mut x = g.text(s(23.0), cy_, &format!("Climb rate (last {nw}W / {nl}L, exact):  "), MUTED, f_mid, 0);
            x = g.text(x, cy_, &format!("win +{win_avg:.1}"), TEXT, f_mid_b, 0);
            x = g.text(x, cy_, "  ·  ", MUTED, f_mid, 0);
            x = g.text(x, cy_, &format!("loss −{loss_avg:.1}"), TEXT, f_mid_b, 0);
            x = g.text(x, cy_, "  ·  difference ", MUTED, f_mid, 0);
            let dc = TEXT;
            x = g.text(x, cy_, &format!("{diff:+.1} LP"), dc, f_mid_b, 0);
            x = g.text(x, cy_, "  →  at 50% WR ", MUTED, f_mid, 0);
            g.text(x, cy_, &format!("{:+.1} LP/game", diff / 2.0), dc, f_mid_b, 0)
        }
        None => g.text(s(23.0), cy_, "Climb rate: needs one exact win and one exact loss", MUTED, f_mid, 0),
    };
    // longest win streak over the whole history of this queue (wins/losses are real even for estimates)
    let x = g.text(climb_end, cy_, "      Longest win streak: ", MUTED, f_mid, 0);
    g.text(x, cy_, &longest_win_streak(&all).to_string(), TEXT, f_mid_b, 0);

    // --- footer ---
    let fy = h - s(34.0);
    let (tc, tt) = if app.tracker_running {
        (WIN, "Tracker running")
    } else {
        (LOSS, "Tracker not running – click to start")
    };
    g.circle(s(32.0), fy + s(8.0), s(4.0), Some(tc), tc, 1);
    let end = g.text(s(42.0), fy, tt, MUTED, f_small, 0);
    app.tracker_btn = rect(s(24.0), fy - s(4.0), end + s(4.0), fy + s(20.0));

    // GitHub backup status (written by the tracker after each sync)
    let (bc, bt) = backup_status(&app.sync);
    let bx = end + s(22.0);
    g.circle(bx, fy + s(8.0), s(4.0), Some(bc), bc, 1);
    g.text(bx + s(10.0), fy, &bt, MUTED, f_small, 0);
    let status = STATUS.lock().unwrap().clone();
    // legend (right)
    let mut lx = w - s(28.0);
    for (label, kind) in [("estimated", 0), ("catch-up", 1), ("exact", 2)] {
        let (lw, _) = g.text_size(label, f_small);
        lx -= lw;
        g.text(lx, fy, label, MUTED, f_small, 0);
        lx -= s(10.0);
        let (cx, cy, r) = (lx, fy + s(8.0), s(4.0));
        match kind {
            0 => g.circle(cx, cy, r, Some(EST), EST, 1),
            1 => {
                g.circle(cx, cy, r, Some(PANEL), WIN, s(2.0).max(2));
            }
            _ => g.circle(cx, cy, r, Some(WIN), WIN, 1),
        }
        lx -= s(18.0);
    }
    if !status.is_empty() {
        g.text(lx, fy, &status, MUTED, f_small, 2);
    }

    // --- plot ---
    let plot = rect(s(96.0), s(132.0), w - s(28.0), h - s(80.0));
    app.plot = plot;
    g.round_rect(rect(plot.left - s(84.0), plot.top - s(12.0), plot.right + s(16.0), plot.bottom + s(36.0)), PANEL, PANEL, s(12.0));

    if pts.is_empty() {
        let msg = if cur.is_some() {
            "No ranked games in this queue yet – they'll appear here after your next game."
        } else {
            "No data yet. Open the League client (and keep lp-tracker running)."
        };
        g.text((plot.left + plot.right) / 2, (plot.top + plot.bottom) / 2 - s(8.0), msg, MUTED, f_mid, 1);
        delete_fonts(&[f_big, f_mid, f_mid_b, f_small, f_sub, f_sub_b]);
        return;
    }

    let ymin = pts.iter().map(|p| p.abs).min().unwrap();
    let ymax = pts.iter().map(|p| p.abs).max().unwrap();
    let mut lo = ((ymin - 25) as f64 / 100.0).floor() as i64 * 100;
    let mut hi = ((ymax + 25) as f64 / 100.0).ceil() as i64 * 100;
    if hi - lo < 200 {
        lo -= 50;
        hi += 50;
        lo = (lo as f64 / 100.0).floor() as i64 * 100;
        hi = (hi as f64 / 100.0).ceil() as i64 * 100;
    }
    lo = lo.max(0);
    let ph = (plot.bottom - plot.top) as f64;
    let pw = (plot.right - plot.left) as f64;
    let ypx = |v: i64| plot.bottom - ((v - lo) as f64 / (hi - lo) as f64 * ph).round() as i32;
    let n = pts.len();
    let xpx = |i: usize| {
        if n == 1 {
            plot.left + (pw / 2.0) as i32
        } else {
            plot.left + (i as f64 * pw / (n - 1) as f64).round() as i32
        }
    };

    // gridlines: one per division (or per tier when zoomed out)
    let step = if (hi - lo) / 100 <= 16 { 100 } else { 400 };
    let first = (lo + step - 1) / step * step;
    let mut v = first;
    while v <= hi {
        let y = ypx(v);
        let tier_line = v % 400 == 0;
        g.line(plot.left, y, plot.right, y, if tier_line { GRID_TIER } else { GRID }, 1, false);
        v += step;
    }
    // labels in the middle of each band
    let mut v = (lo / step) * step;
    while v < hi {
        let mid = v + step / 2;
        if mid > lo && mid < hi {
            let (t, _, _) = rank::from_abs(v);
            let name = if step == 400 { rank::pretty_tier(&t) } else { rank::division_name(v) };
            g.text(plot.left - s(10.0), ypx(mid) - s(8.0), &name, tier_color(&t), f_small, 2);
        }
        v += step;
    }

    // x-axis game numbers
    let tick = [1usize, 2, 5, 10, 20, 25, 50, 100, 200].into_iter().find(|&t| n / t <= 10).unwrap_or(500);
    let mut i = 0;
    while i < n {
        g.text(xpx(i), plot.bottom + s(8.0), &format!("{}", off + i + 1), MUTED, f_small, 1);
        i += tick;
    }

    // "tracking started" divider
    if let Some(k) = pts.iter().position(|p| p.kind != PtKind::Estimated) {
        if k > 0 {
            let x = (xpx(k - 1) + xpx(k)) / 2;
            g.line(x, plot.top, x, plot.bottom, GRID_TIER, 1, true);
            // label right of the line, or left of it when it would run off the plot
            let label = "tracked from here";
            if x + s(6.0) + g.text_size(label, f_small).0 > plot.right {
                g.text(x - s(6.0), plot.top + s(2.0), label, MUTED, f_small, 2);
            } else {
                g.text(x + s(6.0), plot.top + s(2.0), label, MUTED, f_small, 0);
            }
        }
    }

    // line
    let lw = s(2.0).max(1);
    let mut seg: Vec<POINT> = Vec::new();
    let mut seg_est = pts[0].kind == PtKind::Estimated;
    for (i, p) in pts.iter().enumerate() {
        let is_est = p.kind == PtKind::Estimated;
        let pt = POINT { x: xpx(i), y: ypx(p.abs) };
        if is_est != seg_est {
            seg.push(pt);
            g.polyline(&seg, if seg_est { LINE_EST } else { LINE }, lw);
            seg = vec![pt];
            seg_est = is_est;
        } else {
            seg.push(pt);
        }
    }
    g.polyline(&seg, if seg_est { LINE_EST } else { LINE }, lw);

    // markers
    let r = if n > 150 { s(2.5) } else if n > 80 { s(3.0) } else { s(4.0) }.max(2);
    for (i, p) in pts.iter().enumerate() {
        let (cx, cy) = (xpx(i), ypx(p.abs));
        let c = if p.win { WIN } else { LOSS };
        let rr = if app.hover == Some(i) { r + s(2.0) } else { r };
        match p.kind {
            PtKind::Exact => g.circle(cx, cy, rr, Some(c), c, 1),
            PtKind::Catchup => {
                g.circle(cx, cy, rr + 1, Some(PANEL), c, s(2.0).max(2));
            }
            PtKind::Estimated => g.circle(cx, cy, rr, Some(EST), EST, 1),
        }
    }

    // tooltip
    if let Some(i) = app.hover.filter(|&i| i < n) {
        let p = &pts[i];
        let (cx, cy) = (xpx(i), ypx(p.abs));
        g.line(cx, plot.top, cx, plot.bottom, GRID_TIER, 1, true);
        let champ = app.cache.champs.get(&p.champ).cloned().unwrap_or_else(|| {
            if p.champ != 0 { format!("Champion {}", p.champ) } else { "Unknown champion".into() }
        });
        let (t, d, l) = rank::from_abs(p.abs);
        let tag = match p.kind {
            PtKind::Exact => "",
            PtKind::Catchup => "  (catch-up)",
            PtKind::Estimated => "  (estimated)",
        };
        let lines = [
            (format!("Game {}  ·  {}  ·  {}", off + i + 1, champ, if p.win { "Win" } else { "Loss" }), TEXT, f_mid_b),
            (format!("{:+} LP{}", p.delta, tag), if p.delta >= 0 { WIN } else { LOSS }, f_mid),
            (format!("→ {}", rank::label(&t, &d, l)), tier_color(&t), f_mid),
            (fmt_time(p.t), MUTED, f_small),
        ];
        let bw = lines.iter().map(|(t, _, f)| g.text_size(t, *f).0).max().unwrap_or(0) + s(24.0);
        let lh = s(19.0);
        let bh = lh * lines.len() as i32 + s(14.0);
        let mut bx = cx + s(14.0);
        if bx + bw > w - s(8.0) {
            bx = cx - s(14.0) - bw;
        }
        let by = (cy - bh / 2).clamp(s(8.0), h - bh - s(8.0));
        g.round_rect(rect(bx, by, bx + bw, by + bh), BG, GRID_TIER, s(10.0));
        for (k, (t, c, f)) in lines.iter().enumerate() {
            g.text(bx + s(12.0), by + s(8.0) + k as i32 * lh, t, *c, *f, 0);
        }
    }

    delete_fonts(&[f_big, f_mid, f_mid_b, f_small, f_sub, f_sub_b]);
}

fn delete_fonts(f: &[HFONT]) {
    for &x in f {
        unsafe { DeleteObject(x) };
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right, rc.bottom);
        if w > 0 && h > 0 {
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old = SelectObject(mem, bmp);
            APP.with(|a| {
                let mut app = a.borrow_mut();
                let g = Gfx { dc: mem, s: app.scale };
                draw(&g, w, h, &mut app);
            });
            BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
        }
        EndPaint(hwnd, &ps);
    }
}

fn start_backfill(hwnd: HWND) {
    let h = SendPtr(hwnd);
    std::thread::spawn(move || backfill(h));
}

fn watch_files(hwnd: HWND) {
    let h = SendPtr(hwnd);
    std::thread::spawn(move || unsafe {
        let h = h;
        let dir = wide(&store::data_dir().to_string_lossy());
        let ch = FindFirstChangeNotificationW(
            dir.as_ptr(),
            0,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_FILE_NAME,
        );
        if ch == INVALID_HANDLE_VALUE {
            return;
        }
        loop {
            if WaitForSingleObject(ch, INFINITE) != 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(300)); // let the writer finish
            PostMessageW(h.0, WM_RELOAD, 0, 0);
            if FindNextChangeNotification(ch) == 0 {
                return;
            }
        }
    });
}

fn invalidate(hwnd: HWND) {
    unsafe { InvalidateRect(hwnd, null(), 0) };
}

const RANGE_LABELS: [&str; 5] = ["Last 30", "Last 90", "This split", "This season", "All"];

/// Index of the first game shown for the selected range.
fn visible_offset(app: &App) -> usize {
    let pts = &app.series[app.queue];
    match app.range {
        0 => pts.len().saturating_sub(30),
        1 => pts.len().saturating_sub(90),
        // games since the current split started (as reported by the client)
        2 => pts.iter().position(|p| p.t >= app.cache.split_start).unwrap_or(pts.len()),
        3 => pts.iter().position(|p| p.t >= app.cache.season_start).unwrap_or(pts.len()),
        _ => 0,
    }
}

/// Average LP of the last 3 exact wins and last 3 exact losses (estimates and
/// catch-up splits excluded). Returns (win avg, #wins, loss avg as positive, #losses).
fn climb_rate(all: &[Pt]) -> Option<(f64, usize, f64, usize)> {
    let exact = || all.iter().rev().filter(|p| p.kind == PtKind::Exact);
    let wins: Vec<i64> = exact().filter(|p| p.win).take(3).map(|p| p.delta).collect();
    let losses: Vec<i64> = exact().filter(|p| !p.win).take(3).map(|p| -p.delta).collect();
    if wins.is_empty() || losses.is_empty() {
        return None;
    }
    let avg = |v: &[i64]| v.iter().sum::<i64>() as f64 / v.len() as f64;
    Some((avg(&wins), wins.len(), avg(&losses), losses.len()))
}

fn hit_point(app: &App, x: i32, y: i32) -> Option<usize> {
    let pts = &app.series[app.queue][visible_offset(app)..];
    let p = app.plot;
    let n = pts.len();
    if n == 0 || x < p.left - 10 || x > p.right + 10 || y < p.top - 20 || y > p.bottom + 20 {
        return None;
    }
    if n == 1 {
        return Some(0);
    }
    let f = (x - p.left) as f64 / (p.right - p.left) as f64 * (n - 1) as f64;
    Some(f.round().clamp(0.0, (n - 1) as f64) as usize)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_ERASEBKGND => 1,
        WM_SIZE => {
            invalidate(hwnd);
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lp as *mut MINMAXINFO;
            let s = APP.with(|a| a.borrow().scale);
            (*mmi).ptMinTrackSize = POINT { x: (720.0 * s) as i32, y: (420.0 * s) as i32 };
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);
            let changed = APP.with(|a| {
                let mut app = a.borrow_mut();
                let h = hit_point(&app, x, y);
                let c = h != app.hover;
                app.hover = h;
                c
            });
            if changed {
                invalidate(hwnd);
            }
            0
        }
        WM_MOUSELEAVE => {
            APP.with(|a| a.borrow_mut().hover = None);
            invalidate(hwnd);
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, ((lp >> 16) & 0xffff) as i16 as i32);
            let action = APP.with(|a| {
                let mut app = a.borrow_mut();
                for i in 0..2 {
                    if inside(&app.tabs[i], x, y) {
                        app.queue = i;
                        app.hover = None;
                        return 1;
                    }
                }
                for i in 0..5 {
                    if inside(&app.range_btns[i], x, y) {
                        app.range = i;
                        app.hover = None;
                        return 1;
                    }
                }
                if inside(&app.refresh_btn, x, y) {
                    return 2;
                }
                if inside(&app.tracker_btn, x, y) && !app.tracker_running {
                    return 3;
                }
                0
            });
            match action {
                2 => start_backfill(hwnd),
                3 => {
                    start_tracker();
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    APP.with(|a| a.borrow_mut().tracker_running = tracker_running());
                }
                _ => {}
            }
            invalidate(hwnd);
            0
        }
        WM_KEYDOWN => {
            let k = wp as u32;
            if k == b'1' as u32 || k == b'2' as u32 {
                APP.with(|a| {
                    let mut app = a.borrow_mut();
                    app.queue = (k - b'1' as u32) as usize;
                    app.hover = None;
                });
                invalidate(hwnd);
            } else if k == VK_F5 as u32 {
                start_backfill(hwnd);
            }
            0
        }
        WM_TIMER => {
            let changed = APP.with(|a| {
                let mut app = a.borrow_mut();
                let r = tracker_running();
                let c = r != app.tracker_running;
                app.tracker_running = r;
                c
            });
            // timer 2 (once a minute) keeps "backed up … ago" current
            if changed || wp == 2 {
                invalidate(hwnd);
            }
            0
        }
        WM_RELOAD => {
            APP.with(|a| reload(&mut a.borrow_mut()));
            invalidate(hwnd);
            0
        }
        WM_STATUS => {
            invalidate(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        SetProcessDPIAware();
        let screen = GetDC(null_mut());
        let scale = GetDeviceCaps(screen, LOGPIXELSY as _) as f32 / 96.0;
        ReleaseDC(null_mut(), screen);

        APP.with(|a| {
            let mut app = a.borrow_mut();
            app.scale = scale;
            app.tracker_running = tracker_running();
            reload(&mut app);
        });

        let hinst = GetModuleHandleW(null());
        let class = wide("LpGraphWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: common::icon::make(GetSystemMetrics(SM_CXICON)),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("LP Graph").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (1120.0 * scale) as i32,
            (640.0 * scale) as i32,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        let dark: i32 = 1;
        DwmSetWindowAttribute(hwnd, 20 /* DWMWA_USE_IMMERSIVE_DARK_MODE */, &dark as *const i32 as *const c_void, 4);
        let small = common::icon::make(GetSystemMetrics(SM_CXSMICON));
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize);
        ShowWindow(hwnd, SW_SHOW);
        SetTimer(hwnd, 1, 3000, None);
        SetTimer(hwnd, 2, 60_000, None);
        watch_files(hwnd);
        start_backfill(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Days since 1970-01-01 for a (local) date; used for "this week" comparisons.
fn day_number(t: &SYSTEMTIME) -> i64 {
    let (y, m, d) = (t.wYear as i64, t.wMonth as i64, t.wDay as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Most wins in a row among these games.
fn longest_win_streak(pts: &[Pt]) -> usize {
    let (mut best, mut cur) = (0, 0);
    for p in pts {
        cur = if p.win { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}
