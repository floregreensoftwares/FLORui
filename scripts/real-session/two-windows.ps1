# Real-session check for the `two_windows` example: two windows stay
# independent (focus, lifecycle). Needs an interactive Windows desktop, real
# mouse input and UI Automation, so it is not part of `cargo test`.
#
#   cargo build -p florui-example-app --example two_windows
#   pwsh scripts/real-session/two-windows.ps1 target/debug/examples/two_windows.exe
#
# Exits 0 when every check holds, 1 otherwise.
param([Parameter(Mandatory = $true)][string]$Exe)

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type @"
using System; using System.Runtime.InteropServices;
public class RealSession {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, int d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int h2, bool repaint);
}
"@
[RealSession]::SetProcessDPIAware() | Out-Null

$root = [System.Windows.Automation.AutomationElement]::RootElement
$failures = 0
function Check($name, $ok) {
    if ($ok) { "ok    $name" } else { "FAIL  $name"; $script:failures++ }
}
function FindWindow($title) {
    $condition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $title)
    for ($i = 0; $i -lt 60; $i++) {
        $window = $root.FindFirst('Children', $condition)
        if ($window) { return $window }
        Start-Sleep -Milliseconds 500
    }
}
function Descendant($window, $name) {
    $condition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $name)
    $window.FindFirst('Descendants', $condition)
}
function ClickInside($element) {
    $r = $element.Current.BoundingRectangle
    [RealSession]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)) | Out-Null
    Start-Sleep -Milliseconds 300
    [RealSession]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [RealSession]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 900
}

$name = [IO.Path]::GetFileNameWithoutExtension($Exe)
Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
$process = Start-Process $Exe -PassThru
try {
    $a = FindWindow 'Florui -- Window A (config icon)'
    $b = FindWindow 'Florui -- Window B (embedded icon)'
    Check 'both windows open' ($a -and $b)
    # Side by side, so a click lands on the window it aims at.
    [RealSession]::MoveWindow([IntPtr]$a.Current.NativeWindowHandle, 20, 20, 500, 400, $true) | Out-Null
    [RealSession]::MoveWindow([IntPtr]$b.Current.NativeWindowHandle, 560, 20, 500, 400, $true) | Out-Null
    Start-Sleep -Seconds 2

    # Focus is per window: clicking one leaves the other unfocused.
    ClickInside (Descendant $a 'Window A')
    Check 'window A shows Focused after a click on it' ($null -ne (Descendant $a 'Focused'))
    Check 'window B shows Not focused meanwhile' ($null -ne (Descendant $b 'Not focused'))
    ClickInside (Descendant $b 'Window B')
    Check 'window B shows Focused after a click on it' ($null -ne (Descendant $b 'Focused'))
    Check 'window A shows Not focused meanwhile' ($null -ne (Descendant $a 'Not focused'))

    # Changing B's state, then closing A, must leave B open and untouched.
    ClickInside (Descendant $b 'Swap to window A''s icon')
    Check 'window B state changed' ($null -ne (Descendant $b 'Icon updated live -- now matches window A'))
    ClickInside (Descendant $a 'Close window A')
    Start-Sleep -Seconds 1
    $aGone = $null -eq $root.FindFirst('Children', (New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, 'Florui -- Window A (config icon)')))
    Check 'window A closed' $aGone
    Check 'window B still open' (-not $process.HasExited -and $null -ne (FindWindow 'Florui -- Window B (embedded icon)'))
    Check 'window B kept its own state' ($null -ne (Descendant $b 'Icon updated live -- now matches window A'))
}
finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
}
if ($failures -gt 0) { "$failures check(s) failed"; exit 1 }
"all checks passed"
exit 0
