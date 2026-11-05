//! The record file `%APPDATA%\LpGraph\lp.jsonl` (written only by the tracker)
//! and small helpers shared by both programs.

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

/// Data lives in a `data` folder next to the programs (bin\data), not in %APPDATA%:
/// Windows redirects AppData writes of programs started by packaged apps (like the
/// Claude desktop app) into a private folder, which made different copies of the
/// programs see different data. Falls back to %APPDATA%\LpGraph if bin isn't writable.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("data"))) {
        if std::fs::create_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let d = base.join("LpGraph");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn records_path() -> PathBuf {
    data_dir().join("lp.jsonl")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Snapshot with no game attached (first run, or LP changed without a game, e.g. decay).
    Baseline,
    /// LP change of one game, read right after it ended.
    Exact,
    /// One of several games missed while the tracker was off: total exact, split estimated.
    Catchup,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Baseline => "baseline",
            Kind::Exact => "exact",
            Kind::Catchup => "catchup",
        }
    }
    fn parse(s: &str) -> Kind {
        match s {
            "exact" => Kind::Exact,
            "catchup" => Kind::Catchup,
            _ => Kind::Baseline,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    pub t: i64,
    pub queue: String,
    pub tier: String,
    pub div: String,
    pub lp: i64,
    pub wins: i64,
    pub losses: i64,
    pub abs: i64,
    pub delta: i64,
    pub game_id: i64,
    pub champ: i64,
    pub win: bool,
    pub kind: Kind,
}

impl Record {
    pub fn to_line(&self) -> String {
        json!({
            "t": self.t, "queue": self.queue, "tier": self.tier, "div": self.div, "lp": self.lp,
            "w": self.wins, "l": self.losses, "abs": self.abs, "delta": self.delta,
            "gameId": self.game_id, "champ": self.champ, "win": self.win, "kind": self.kind.as_str(),
        })
        .to_string()
    }

    pub fn from_line(line: &str) -> Option<Record> {
        let v: Value = serde_json::from_str(line).ok()?;
        Some(Record {
            t: v["t"].as_i64()?,
            queue: v["queue"].as_str()?.to_string(),
            tier: v["tier"].as_str().unwrap_or("").to_string(),
            div: v["div"].as_str().unwrap_or("").to_string(),
            lp: v["lp"].as_i64().unwrap_or(0),
            wins: v["w"].as_i64().unwrap_or(0),
            losses: v["l"].as_i64().unwrap_or(0),
            abs: v["abs"].as_i64()?,
            delta: v["delta"].as_i64().unwrap_or(0),
            game_id: v["gameId"].as_i64().unwrap_or(0),
            champ: v["champ"].as_i64().unwrap_or(0),
            win: v["win"].as_bool().unwrap_or(false),
            kind: Kind::parse(v["kind"].as_str().unwrap_or("")),
        })
    }

    pub fn is_game(&self) -> bool {
        self.kind != Kind::Baseline
    }
}

pub fn load_records() -> Vec<Record> {
    std::fs::read_to_string(records_path())
        .map(|s| s.lines().filter_map(Record::from_line).collect())
        .unwrap_or_default()
}

pub fn append_records(recs: &[Record]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(records_path())?;
    let mut s = String::new();
    for r in recs {
        s.push_str(&r.to_line());
        s.push('\n');
    }
    f.write_all(s.as_bytes())
}

/// Average LP gained per win and lost per loss (both positive) from exact records.
pub fn averages(records: &[Record], queue: &str) -> (f64, f64) {
    let (mut ws, mut wn, mut ls, mut ln) = (0i64, 0i64, 0i64, 0i64);
    for r in records.iter().filter(|r| r.queue == queue && r.kind == Kind::Exact) {
        if r.win && r.delta > 0 {
            ws += r.delta;
            wn += 1;
        } else if !r.win && r.delta < 0 {
            ls += -r.delta;
            ln += 1;
        }
    }
    let w = if wn > 0 { ws as f64 / wn as f64 } else { 22.0 };
    let l = if ln > 0 { ls as f64 / ln as f64 } else { 18.0 };
    (w, l)
}

/// Split an exact total LP change over several games by their win/loss pattern.
pub fn split_delta(total: i64, wins: &[bool], avg: (f64, f64)) -> Vec<i64> {
    if wins.is_empty() {
        return Vec::new();
    }
    let est: Vec<f64> = wins.iter().map(|&w| if w { avg.0 } else { -avg.1 }).collect();
    let resid = (total as f64 - est.iter().sum::<f64>()) / wins.len() as f64;
    let mut out: Vec<i64> = est.iter().map(|e| (e + resid).round() as i64).collect();
    // fix rounding so the parts add up exactly
    let diff = total - out.iter().sum::<i64>();
    if let Some(last) = out.last_mut() {
        *last += diff;
    }
    out
}
