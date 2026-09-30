# Screenshot every canned GUI state in both themes (demo mode only).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/shots.ps1 [-States a,b] [-Themes light,dark]
param(
  [string[]]$States = @("idle", "sharing-waiting", "sharing-here", "sharing-remote", "browsing",
    "client-connected", "client-remote", "error", "keys", "keys-confirm", "settings"),
  [string[]]$Themes = @("light", "dark"),
  [string]$Exe = "target/debug/input-share-gui.exe",
  [string]$OutDir = "docs/gui"
)
New-Item -ItemType Directory -Force $OutDir | Out-Null
# Under `powershell -File`, "a,b" arrives as one string, so split it here.
$States = $States -split ","
$Themes = $Themes -split ","
foreach ($t in $Themes) {
  foreach ($s in $States) {
    $out = Join-Path $OutDir "$s-$t.png"
    & "$PSScriptRoot/shot.ps1" -Exe $Exe -ArgList "--demo --demo-state $s --demo-theme $t" -Out $out -WaitMs 2500
  }
}
