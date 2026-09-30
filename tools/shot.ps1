# Launch an exe, capture only its main window with PrintWindow, save a PNG, close it.
# Never captures the rest of the screen.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools/shot.ps1 `
#          -Exe target/debug/input-share-gui.exe -ArgList "--demo --demo-state remote" -Out docs/gui/remote.png
param([string]$Exe, [string]$ArgList = "", [string]$Out, [int]$WaitMs = 4000)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
}
"@

if ($ArgList) { $p = Start-Process -FilePath $Exe -ArgumentList $ArgList -PassThru }
else { $p = Start-Process -FilePath $Exe -PassThru }
try {
  $deadline = (Get-Date).AddMilliseconds($WaitMs + 10000)
  while ($p.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200; $p.Refresh() }
  if ($p.MainWindowHandle -eq 0) { throw "no window appeared" }
  Start-Sleep -Milliseconds $WaitMs   # let the webview render
  # The first handle can be a tiny transient window; wait for the real one.
  $r = New-Object W+RECT
  do {
    $p.Refresh()
    [void][W]::GetWindowRect($p.MainWindowHandle, [ref]$r)
    if (($r.R - $r.L) -ge 100) { break }
    Start-Sleep -Milliseconds 200
  } while ((Get-Date) -lt $deadline)
  if (($r.R - $r.L) -lt 100) { throw "window never reached a real size ($($r.R - $r.L) px wide)" }
  $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  # 2 = PW_RENDERFULLCONTENT, needed for DirectComposition content like WebView2
  $ok = [W]::PrintWindow($p.MainWindowHandle, $hdc, 2)
  $g.ReleaseHdc($hdc)
  $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
  "captured=$ok size=$($bmp.Width)x$($bmp.Height)"
} finally {
  Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
}
