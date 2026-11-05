//! Pushes the tracker data (bin/data) to the GitHub repo after each recorded game.
//! Runs git hidden on a background thread; failures are logged and retried with the next game.

use common::store;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static BUSY: Mutex<()> = Mutex::new(());

fn marker() -> PathBuf {
    store::data_dir().join("no-sync")
}

pub fn enabled() -> bool {
    !marker().exists()
}

pub fn set_enabled(on: bool) {
    if on {
        let _ = std::fs::remove_file(marker());
    } else {
        let _ = std::fs::write(marker(), b"");
    }
}

/// The git repo that contains the data folder (bin/data -> repo root), if any.
fn repo_root() -> Option<PathBuf> {
    let mut d = store::data_dir();
    loop {
        if d.join(".git").exists() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0") // never wait for a password
        .env("GCM_INTERACTIVE", "never")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("git not found: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string() + &String::from_utf8_lossy(&out.stderr).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// Commits bin/data with `message` (if anything changed) and pushes. Runs in the background.
/// With `message` = None it only pushes commits that haven't been pushed yet.
pub fn push_async(message: Option<String>, log: fn(&str)) {
    if !enabled() {
        return;
    }
    std::thread::spawn(move || {
        let _guard = BUSY.lock();
        let Some(root) = repo_root() else { return };
        if let Some(msg) = message {
            let data = store::data_dir();
            let rel = data.strip_prefix(&root).unwrap_or(&data).to_string_lossy().replace('\\', "/");
            if let Err(e) = git(&root, &["add", "--", &rel]) {
                log(&format!("sync: git add failed: {e}"));
                return;
            }
            // exit code 1 = there are staged changes
            if git(&root, &["diff", "--cached", "--quiet"]).is_err() {
                if let Err(e) = git(&root, &["commit", "-q", "-m", &msg]) {
                    log(&format!("sync: commit failed: {e}"));
                    return;
                }
            }
        }
        // only push when there are local commits that aren't on GitHub yet
        let ahead = git(&root, &["rev-list", "--count", "@{u}..HEAD"]).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(1);
        if ahead == 0 {
            return;
        }
        match git(&root, &["push", "-q"]) {
            Ok(_) => log(&format!("sync: pushed {ahead} commit(s) to GitHub")),
            Err(e) => log(&format!("sync: push failed (will retry after the next game): {e}")),
        }
    });
}
