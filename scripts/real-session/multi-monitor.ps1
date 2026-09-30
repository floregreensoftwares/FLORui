# Real-session check for monitors with different scale factors: moves the
# `counter` example across every monitor, then checks that its button is drawn
# at the scale of the monitor it is on and that a real click still lands on it.
# Needs an interactive Windows desktop with at least two monitors at different
# scales (Settings > System > Display > Scale), so it is not part of CI.
#
#   cargo build -p florui-example-app --example counter
#   pwsh scripts/real-session/multi-monitor.ps1 target/debug/examples/counter.exe
#
# Exits 0 when every check holds, 1 on a failure, 2 when the setup has no
# mixed scales to test.
param([Parameter(Mandatory = $true)][string]$Exe)
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type @"
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public class MultiMonitor {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int l, t, r, b; }
  public delegate bool Enum(IntPtr h, IntPtr dc, ref RECT r, IntPtr d);
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern bool EnumDisplayMonitors(IntPtr dc, IntPtr clip, Enum cb, IntPtr d);
  [DllImport("shcore.dll")] public static extern int GetDpiForMonitor(IntPtr h, int t, out uint x, out uint y);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int h2, bool r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, int d, UIntPtr e);
  public static List<int[]> Monitors() {
    var list = new List<int[]>();
    EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (IntPtr h, IntPtr dc, ref RECT r, IntPtr d) => {
      uint x, y; GetDpiForMonitor(h, 0, out x, out y);
      list.Add(new int[] { r.l, r.t, (int)x });
      return true; }, IntPtr.Zero);
    return list;
  }
}
"@
[MultiMonitor]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null

$monitors = [MultiMonitor]::Monitors()
foreach ($m in $monitors) { "monitor at ($($m[0]),$($m[1])) scale $([int]($m[2] * 100 / 96))%" }
if (($monitors | ForEach-Object { $_[2] } | Sort-Object -Unique).Count -lt 2) {
    "needs two monitors at different scales"; exit 2
}

$root = [System.Windows.Automation.AutomationElement]::RootElement
$failures = 0
function Check($name, $ok) {
    if ($ok) { "ok    $name" } else { "FAIL  $name"; $script:failures++ }
}
function Descendant($window, $name) {
    $window.FindFirst('Descendants', (New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $name)))
}

Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($Exe)) -ErrorAction SilentlyContinue | Stop-Process -Force
$process = Start-Process $Exe -PassThru
try {
    $window = $null
    for ($i = 0; $i -lt 40 -and -not $window; $i++) {
        $window = $root.FindFirst('Children', (New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'Florui counter')))
        Start-Sleep -Milliseconds 500
    }
    Check 'window opens' ($null -ne $window)
    $handle = [IntPtr]$window.Current.NativeWindowHandle

    $baseWidth = $null
    $clicks = 0
    # A second lap returns to the first monitor, so a scale change in both directions is covered.
    $lap = New-Object System.Collections.ArrayList
    $monitors | ForEach-Object { [void]$lap.Add($_) }
    [void]$lap.Add($monitors[0])
    foreach ($m in $lap) {
        $scale = $m[2] / 96
        [MultiMonitor]::MoveWindow($handle, $m[0] + 100, $m[1] + 100, [int](600 * $scale), [int](500 * $scale), $true) | Out-Null
        Start-Sleep -Seconds 2
        [MultiMonitor]::SetForegroundWindow($handle) | Out-Null

        # The rectangle is briefly infinite while the window settles on the new monitor.
        for ($i = 0; $i -lt 20; $i++) {
            $button = Descendant $window '+1'
            $r = $button.Current.BoundingRectangle
            if (-not [double]::IsInfinity($r.Width) -and $r.Width -gt 0) { break }
            Start-Sleep -Milliseconds 250
        }
        # Logical width = physical width / scale; it must not depend on the monitor.
        $logical = $r.Width / $scale
        if ($null -eq $baseWidth) { $baseWidth = $logical }
        Check "scale $([int]($scale * 100))% at ($($m[0]),$($m[1])): button is $([int]$logical) logical px wide" ([math]::Abs($logical - $baseWidth) -le 1.5)

        [MultiMonitor]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)) | Out-Null
        Start-Sleep -Milliseconds 300
        [MultiMonitor]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
        [MultiMonitor]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 600
        $clicks++
        Check "a real click lands on the button ($clicks so far)" ($null -ne (Descendant $window "x2 = $($clicks * 2)"))
    }
}
finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
}
if ($failures -gt 0) { "$failures check(s) failed"; exit 1 }
"all checks passed"
exit 0
