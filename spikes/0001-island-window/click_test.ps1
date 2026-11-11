param([string]$Mode = "canvas", [string]$Out)
# Notepad must be a classic Win32 window: on stock Windows 11 the packaged Notepad
# may return MainWindowHandle = 0. Install classic Notepad or swap in another app.
$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "target/debug/spike-island-window.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -TypeDefinition @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int hh, bool r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public static string Fg() { var s = new StringBuilder(256); GetWindowText(GetForegroundWindow(), s, 256); return s.ToString(); }
  public static void Click(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(120); mouse_event(2,0,0,0,UIntPtr.Zero); mouse_event(4,0,0,0,UIntPtr.Zero); }
}
'@
function Shot($name, $x, $y, $w, $h) {
  $b = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($x, $y, 0, 0, $b.Size); $g.Dispose()
  $b.Save((Join-Path $Out "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()
}

# Notepad under the canvas, with known text so transparency is visible.
$np = Start-Process notepad -PassThru
Start-Sleep 2
$np.Refresh()
[W]::MoveWindow($np.MainWindowHandle, 560, 0, 800, 500, $true) | Out-Null
[W]::SetForegroundWindow($np.MainWindowHandle) | Out-Null
Start-Sleep 1
Shot "0-before" 500 0 920 560

$env:SPIKE_MODE = $Mode
$log = Join-Path $Out "spike.log"
$p = Start-Process $exe -PassThru -RedirectStandardOutput $log -RedirectStandardError (Join-Path $Out "spike.err") -WindowStyle Hidden
Start-Sleep 6
Shot "1-idle" 500 0 920 560
"foreground after start: '$([W]::Fg())'"

$cpu0 = (Get-Process -Id $p.Id).TotalProcessorTime
$wv = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.Id)" | ForEach-Object { $_.ProcessId }
$wvAll = @($wv) + @(foreach ($id in $wv) { Get-CimInstance Win32_Process -Filter "ParentProcessId=$id" | ForEach-Object { $_.ProcessId } })
$wv0 = ($wvAll | ForEach-Object { (Get-Process -Id $_ -ErrorAction SilentlyContinue).TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
Start-Sleep 10
$cpu1 = (Get-Process -Id $p.Id).TotalProcessorTime
$wv1 = ($wvAll | ForEach-Object { (Get-Process -Id $_ -ErrorAction SilentlyContinue).TotalProcessorTime.TotalMilliseconds } | Measure-Object -Sum).Sum
"idle cpu 10s: host_ms=$([int]($cpu1-$cpu0).TotalMilliseconds) webview_ms=$([int]($wv1-$wv0)) cores=$([Environment]::ProcessorCount)"

# Click in transparent canvas area (over Notepad text, outside the island).
[W]::Click(700, 300); Start-Sleep 1
"click transparent (700,300): foreground='$([W]::Fg())'"

# Click on the island itself (centre of idle island at top centre).
[W]::Click(960, 36); Start-Sleep 1
"click island (960,36): foreground='$([W]::Fg())'"
Start-Sleep 0.25
Shot "2-mid-transition" 500 0 920 560
Start-Sleep 1
Shot "3-listening" 500 0 920 560

# Click just outside the island edge, below it.
[W]::Click(960, 80); Start-Sleep 1
"click below island (960,80): foreground='$([W]::Fg())'"

# Cycle to awaiting_confirm (3 more island clicks) and click inside the large card.
foreach ($i in 1..3) { [W]::Click(960, 40); Start-Sleep 1 }
Shot "4-confirm" 500 0 920 560
[W]::Click(960, 250); Start-Sleep 1
"click inside confirm card (960,250): foreground='$([W]::Fg())'"
[W]::Click(1300, 400); Start-Sleep 1
"click transparent (1300,400): foreground='$([W]::Fg())'"

Start-Sleep 11
Stop-Process -Id $p.Id -Force
Stop-Process -Id $np.Id -Force
Start-Sleep 1
"--- spike.log ---"
Get-Content $log
"--- spike.err (tail) ---"
Get-Content (Join-Path $Out "spike.err") -Tail 10

