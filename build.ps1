# Builds both programs, writes the icon + name into them, builds the one-file installer
# (dist\LP-Graph-Setup.exe), and installs the programs into bin\ for development.
# Usage:  .\build.ps1
# (no $ErrorActionPreference = "Stop": PowerShell 5.1 would treat cargo's progress output on
# stderr as an error; exit codes are checked instead)
Set-Location $PSScriptRoot

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# icon and version info ("LP Tracker" / "LP Graph") for Explorer, Task Manager and notifications
.\target\release\stamp.exe target\release\lp-tracker.exe "LP Tracker" target\release\lp-graph.exe "LP Graph"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# one-file installer with both (stamped) programs inside -> dist\LP-Graph-Setup.exe
cargo build --release -p lp-graph-setup
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\LP-Graph-Setup.exe dist -Force
.\target\release\stamp.exe dist\LP-Graph-Setup.exe "LP Graph Setup"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# the programs can't be replaced while they run: stop them, copy, start the tracker again
$wasRunning = [bool](Get-Process lp-tracker -ErrorAction SilentlyContinue)
Get-Process lp-tracker, lp-graph -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
New-Item -ItemType Directory -Force bin | Out-Null
Copy-Item target\release\lp-tracker.exe, target\release\lp-graph.exe bin -Force
Write-Host "Installed to bin\"
if ($wasRunning) {
    Start-Process explorer.exe (Resolve-Path bin\lp-tracker.exe)
    Write-Host "Tracker restarted"
}
