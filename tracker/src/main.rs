//! lp-tracker: background process with a tray icon. Starts with Windows, waits for the
//! League client, listens to its WebSocket and appends LP changes to lp.jsonl.
#![windows_subsystem = "windows"]

use common::rank::{from_abs, standing, Standing, QUEUES};
use common::store::{self, Kind, Record};
use common::{find_process, http::WebSocket, now_ms, wide, Game, Lcu, CLIENT_EXE, TRACKER_MUTEX};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::thread::sleep;
use std::time::Duration;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

mod sync;
mod tray;
use windows_sys::Win32::System::Threading::CreateMutexW;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "LpTracker";
const TOPICS: [&str; 3] = [
    "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase",
    "OnJsonApiEvent_lol-ranked_v1_current-ranked-stats",
    "OnJsonApiEvent_lol-ranked_v1_current-lp-change-notification",
];

fn log(msg: &str) {
    let path = store::data_dir().join("tracker.log");
    if std::fs::metadata(&path).map(|m| m.len() > 64 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", now_ms() / 1000, msg);
    }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let data = wide(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(RUN_VALUE).as_ptr(),
                REG_SZ,
                data.as_ptr() as *const _,
                (data.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(RUN_VALUE).as_ptr());
        }
    }
}

fn autostart_on() -> bool {
    unsafe {
        let mut size: u32 = 0;
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_VALUE).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) == 0
    }
}

/// Menu toggle. Turning it off is remembered so the tracker doesn't re-enable it on its next start.
fn toggle_autostart(on: bool) {
    let marker = store::data_dir().join("no-autostart");
    set_autostart(on);
    if on {
        let _ = std::fs::remove_file(marker);
    } else {
        let _ = std::fs::write(marker, b"");
    }
}

/// Tray toggle: switch sync on/off and refresh the footer status right away.
fn toggle_sync(on: bool) {
    sync::set_enabled(on);
    sync::push_async(None, log);
}

fn main() {
    if std::env::args().any(|a| a == "--uninstall") {
        toggle_autostart(false);
        return;
    }
    // Single instance. The handle stays open for the life of the process.
    let _mutex = unsafe { CreateMutexW(std::ptr::null(), 0, wide(TRACKER_MUTEX).as_ptr()) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    if !store::data_dir().join("no-autostart").exists() {
        set_autostart(true); // also keeps the path current if the exe was moved
    }
    log("tracker started");
    sync::push_async(None, log); // push anything left over from last time
    if let Some(r) = store::load_records().last() {
        tray::set_last(&format!("Last: {}", common::rank::label(&r.tier, &r.div, r.lp)));
    }

    std::thread::spawn(|| loop {
        if find_process(CLIENT_EXE).is_none() {
            tray::set_status("Waiting for League client");
            sleep(Duration::from_secs(15));
            continue;
        }
        if let Some(lcu) = Lcu::find() {
            session(&lcu);
        }
        sleep(Duration::from_secs(5));
    });
    tray::run(tray::Hooks { autostart_on, set_autostart: toggle_autostart, sync_on: sync::enabled, set_sync: toggle_sync });
}

/// One connection to a running client. Returns when the client goes away.
fn session(lcu: &Lcu) {
    // The API only answers once the player is logged in.
    let stats = loop {
        if let Some(s) = lcu.ranked_stats() {
            if s["queueMap"].is_object() {
                break s;
            }
        }
        if find_process(CLIENT_EXE).is_none() {
            return;
        }
        sleep(Duration::from_secs(5));
    };
    let mut records = store::load_records();
    check(lcu, &mut records, Some(stats)); // catch up on games played while we were off

    let ws = match WebSocket::connect(lcu.port, &lcu.auth_header) {
        Ok(ws) => ws,
        Err(e) => {
            log(&format!("websocket: {e}"));
            return;
        }
    };
    for t in TOPICS {
        ws.send_text(&format!("[5,\"{t}\"]"));
    }
    log("connected to client");
    tray::set_status("Connected to League client");

    while let Some(msg) = ws.recv_text() {
        let Ok(v) = serde_json::from_str::<Value>(&msg) else { continue };
        let ev = &v[2];
        match ev["uri"].as_str().unwrap_or("") {
            "/lol-gameflow/v1/gameflow-phase" => {
                if matches!(ev["data"].as_str(), Some("PreEndOfGame" | "WaitingForStats" | "EndOfGame")) {
                    // LP usually updates within a few seconds of the end screen.
                    for _ in 0..15 {
                        if check(lcu, &mut records, None) {
                            break;
                        }
                        sleep(Duration::from_secs(2));
                    }
                }
            }
            "/lol-ranked/v1/current-ranked-stats" => {
                let data = ev["data"].clone();
                let stats = if data["queueMap"].is_object() { Some(data) } else { None };
                check(lcu, &mut records, stats);
            }
            "/lol-ranked/v1/current-lp-change-notification" => {
                check(lcu, &mut records, None);
            }
            _ => {}
        }
    }
    log("client disconnected");
    tray::set_status("Waiting for League client");
}

/// Compares current ranked stats with the last record per queue and appends
/// whatever changed. Returns true if anything was recorded.
fn check(lcu: &Lcu, records: &mut Vec<Record>, stats: Option<Value>) -> bool {
    let Some(stats) = stats.or_else(|| lcu.ranked_stats()) else { return false };
    let mut new = Vec::new();
    for (queue, qid, _) in QUEUES {
        let Some(s) = standing(&stats, queue) else { continue };
        let last = records.iter().rev().find(|r| r.queue == queue).cloned();
        match last {
            None => new.push(snapshot(queue, &s, Kind::Baseline, 0, 0, false, 0)),
            Some(l) => {
                let games = (s.wins + s.losses) - (l.wins + l.losses);
                if games <= 0 {
                    // No new game. Record LP changes without games (decay, season reset).
                    if s.abs != l.abs || games < 0 {
                        new.push(snapshot(queue, &s, Kind::Baseline, s.abs - l.abs, 0, false, 0));
                    }
                    continue;
                }
                new.extend(game_records(lcu, records, queue, qid, &l, &s, games));
            }
        }
    }
    if new.is_empty() {
        return false;
    }
    for r in &new {
        log(&format!("recorded {} {} {:+} -> {} {} {}", r.kind.as_str(), r.queue, r.delta, r.tier, r.div, r.lp));
    }
    if let Some(r) = new.iter().rev().find(|r| r.is_game()) {
        tray::set_last(&format!("Last game: {:+} LP → {}", r.delta, common::rank::label(&r.tier, &r.div, r.lp)));
    }
    if let Err(e) = store::append_records(&new) {
        log(&format!("write failed: {e}"));
    }
    sync::push_async(Some(commit_message(&new)), log);
    records.extend(new);
    true
}

/// e.g. "Solo/Duo: +23 LP (Win) → Gold II 68 LP"
fn commit_message(new: &[Record]) -> String {
    let Some(last) = new.last() else { return "Update LP data".into() };
    let queue = QUEUES.iter().find(|q| q.0 == last.queue).map(|q| q.2).unwrap_or("Ranked");
    let rank = common::rank::label(&last.tier, &last.div, last.lp);
    let games: Vec<&Record> = new.iter().filter(|r| r.is_game()).collect();
    match games.len() {
        0 => format!("{queue}: LP update → {rank}"),
        1 => format!("{queue}: {:+} LP ({}) → {rank}", games[0].delta, if games[0].win { "Win" } else { "Loss" }),
        n => format!("{queue}: {n} games, {:+} LP → {rank}", games.iter().map(|r| r.delta).sum::<i64>()),
    }
}

fn snapshot(queue: &str, s: &Standing, kind: Kind, delta: i64, game_id: i64, win: bool, champ: i64) -> Record {
    Record {
        t: now_ms(),
        queue: queue.into(),
        tier: s.tier.clone(),
        div: s.div.clone(),
        lp: s.lp,
        wins: s.wins,
        losses: s.losses,
        abs: s.abs,
        delta,
        game_id,
        champ,
        win,
        kind,
    }
}

/// Builds records for `n` games played since `last`.
fn game_records(lcu: &Lcu, records: &[Record], queue: &str, qid: i64, last: &Record, s: &Standing, n: i64) -> Vec<Record> {
    let n = n as usize;
    let known: HashSet<i64> = records.iter().map(|r| r.game_id).filter(|&id| id != 0).collect();
    // Find the matching games in match history (it can lag a few seconds behind).
    let mut games: Vec<Game> = Vec::new();
    for attempt in 0..4 {
        games = lcu
            .match_history(0, 200) // same range as lp-graph: the client keeps the first range it was asked for
            .into_iter()
            .filter(|g| g.queue_id == qid && g.counts() && !known.contains(&g.id) && g.t > last.t - 60_000)
            .take(n)
            .collect();
        if games.len() >= n || attempt == 3 {
            break;
        }
        sleep(Duration::from_secs(2));
    }
    games.reverse(); // oldest first

    let dw = (s.wins - last.wins).max(0) as usize;
    let wins: Vec<bool> = if games.len() == n {
        games.iter().map(|g| g.win).collect()
    } else {
        (0..n).map(|i| i < dw).collect()
    };
    let total = s.abs - last.abs;
    let deltas = if n == 1 { vec![total] } else { store::split_delta(total, &wins, store::averages(records, queue)) };
    let kind = if n == 1 { Kind::Exact } else { Kind::Catchup };

    let mut out = Vec::new();
    let (mut abs, mut w, mut l) = (last.abs, last.wins, last.losses);
    for i in 0..n {
        abs += deltas[i];
        if wins[i] { w += 1 } else { l += 1 }
        let g = games.get(i);
        let mut st = if i + 1 == n {
            s.clone()
        } else {
            let (tier, div, lp) = from_abs(abs);
            Standing { tier, div, lp, wins: w, losses: l, abs }
        };
        st.wins = w;
        st.losses = l;
        let mut r = snapshot(queue, &st, kind, deltas[i], g.map(|g| g.id).unwrap_or(0), wins[i], g.map(|g| g.champ).unwrap_or(0));
        if let Some(g) = g {
            r.t = r.t.min(g.t + g.duration * 1000).max(last.t);
        }
        out.push(r);
    }
    out
}
