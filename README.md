# LP Graph

A lightweight native Windows tool (Rust, raw Win32 + GDI) that graphs your League of Legends LP game by game, using only the League client's local API (LCU).

It is two programs:

- **`lp-tracker.exe`** runs in the tray and starts with Windows. It listens to the client's WebSocket and records your exact LP change at the end of every ranked game.
- **`lp-graph.exe`** is the graph window: a game-by-game LP line with Solo/Duo and Flex, ranges (Last 30, Last 90, This split, This season, All), daily and weekly LP, a climb rate (your average win and loss LP over your last 3 of each) and your longest win streak.

Your Riot match history doesn't include LP, so games played before the tracker ran are estimated by walking backwards from your current LP. On the graph:

| Marker | Meaning |
| --- | --- |
| Filled green or red dot | Exact: recorded at the end screen |
| Ring | Catch-up: one of several games missed while the tracker was off (the total is exact, the split per game is approximate) |
| Gray dot | Estimated from match history |

## Install

Download **`LP-Graph-Setup.exe`** from the [latest release](https://github.com/wivdvo/lp-graph/releases/latest) and run it. That's the only file you need:

- It installs both programs to `%LOCALAPPDATA%\Programs\LP Graph` (no admin rights needed), adds **LP Graph** to the Start Menu, starts the tracker and opens the graph.
- The tracker starts with Windows (turn this off in its tray menu).
- Running a newer setup over it updates the programs and keeps your data.
- Uninstall from Windows' **Settings → Apps → Installed apps → LP Graph**. It asks whether to keep your LP history.

The programs aren't code-signed, so Windows SmartScreen may warn the first time ("More info" → "Run anyway").

Setup options: `--quiet` (no dialogs), `--no-start`, `--dir <folder>`; `uninstall.exe --uninstall [--quiet]`.

## Build

```powershell
.\build.ps1
```

This runs `cargo build --release`, then `tools/stamp` writes the icon and the names "LP Tracker" / "LP Graph" into the two `.exe` files (using Windows' own resource API, so no resource compiler is needed). It then builds `installer/` with both programs embedded into `dist\LP-Graph-Setup.exe`, and copies the programs into `bin/` for development.

## Data

Everything is stored in `bin/data/`, next to the programs:

- `lp.jsonl`: **all your ranked games**, one line each: game ID, champion, win/loss, queue, start time, duration, LP change and rank after the game. Lines are `exact` or `catchup` (recorded by the tracker) or `estimated` (games from before the tracker existed: real result, estimated LP). This is the only file that matters and the only data file in git.
- `history_cache.json`: a local copy of the client's match history, champion names and season dates. It can be rebuilt from the client at any time, so it isn't committed.
- `tracker.log`: tracker log, capped at 64 KB. Not committed.

After each recorded game the tracker commits `lp.jsonl` and pushes it to GitHub (tray menu: "Sync to GitHub after each game").

The data lives next to the programs instead of in `%APPDATA%` because Windows redirects AppData writes for programs started by packaged apps, which made different copies of the programs see different data.
