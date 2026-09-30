# Real-session check that the inspector does not change what it inspects: runs
# the same mouse, keyboard and wheel script against `inspector_menu` once with
# the inspector window open and once with the plain desktop host, records what
# the application did after each step (menu, focus, typed text, scroll), and
# fails if the two runs differ. Needs an interactive Windows desktop, so it is
# not part of CI.
#
#   cargo build -p florui-example-app --example inspector_menu
#   pwsh scripts/real-session/inspector.ps1 target/debug/examples/inspector_menu.exe
#
# Exits 0 when both runs match, 1 otherwise.
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    # Diagnostic: wait this long after start instead of waiting for idle.
    [int]$FixedWaitSeconds = 0
)
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public class InspectorCheck {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int h2, bool r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, int d, UIntPtr e);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr e);
}
"@
[InspectorCheck]::SetProcessDpiAwarenessContext([IntPtr]-4) | Out-Null
$AE = [System.Windows.Automation.AutomationElement]
$root = $AE::RootElement

function FindWindow($title) {
    $condition = New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $title)
    for ($i = 0; $i -lt 60; $i++) {
        $window = $root.FindFirst('Children', $condition)
        if ($window) { return $window }
        Start-Sleep -Milliseconds 500
    }
}
function Descendant($window, $name) {
    $window.FindFirst('Descendants', (New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $name)))
}
function Center($element) {
    $r = $element.Current.BoundingRectangle
    @([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
}
function Click($element) {
    $c = Center $element
    [InspectorCheck]::SetCursorPos($c[0], $c[1]) | Out-Null
    Start-Sleep -Milliseconds 300
    [InspectorCheck]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    [InspectorCheck]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 700
}
function Press($vk) {
    [InspectorCheck]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    [InspectorCheck]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}
# The text field's border is red once the field shows :user-invalid.
function Border($app) {
    $field = $app.FindFirst('Descendants', (New-Object System.Windows.Automation.PropertyCondition(
        $AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)))
    $r = $field.Current.BoundingRectangle
    $bitmap = New-Object System.Drawing.Bitmap(1, 1)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen([int]($r.X + $r.Width / 2), [int]($r.Y + 1), 0, 0, $bitmap.Size)
    $pixel = $bitmap.GetPixel(0, 0)
    $graphics.Dispose(); $bitmap.Dispose()
    if ($pixel.R -gt 200 -and $pixel.G -lt 80) { 'red' } else { 'normal' }
}

function State($app) {
    $names = @($app.FindAll('Descendants', [System.Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object { $_.Current.Name })
    $facts = @('picked:', 'typed:', 'scrolled:') | ForEach-Object {
        $prefix = $_
        ($names | Where-Object { $_ -like "$prefix*" } | Select-Object -First 1)
    }
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name
    $menu = $null -ne (Descendant $app 'Item one')
    "menu=$menu focus='$focused' $($facts -join ' | ')"
}

# Waits until the process has stopped using the CPU: its first frames are
# drawn and it is idle, so input sent now is handled when it is sent.
function WaitIdle($process, $quietMilliseconds = 1000, $timeoutSeconds = 30) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    $quietSince = $null
    $last = $process.TotalProcessorTime
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        $now = $process.TotalProcessorTime
        if (($now - $last).TotalMilliseconds -lt 15) {
            if (-not $quietSince) { $quietSince = Get-Date }
            if (((Get-Date) - $quietSince).TotalMilliseconds -ge $quietMilliseconds) { return }
        } else { $quietSince = $null }
        $last = $now
    }
    throw "$($process.ProcessName) never went idle"
}

function Run($extraArgs, $withInspector) {
    $name = [IO.Path]::GetFileNameWithoutExtension($Exe)
    Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
    $process = if ($extraArgs) { Start-Process $Exe -ArgumentList $extraArgs -PassThru } else { Start-Process $Exe -PassThru }
    $trace = @()
    try {
        $app = FindWindow 'Florui inspector menu'
        $inspector = if ($withInspector) { FindWindow 'Florui inspector' } else { $null }
        $h = [IntPtr]$app.Current.NativeWindowHandle
        [InspectorCheck]::MoveWindow($h, 40, 40, 600, 700, $true) | Out-Null
        if ($inspector) {
            [InspectorCheck]::MoveWindow([IntPtr]$inspector.Current.NativeWindowHandle, 680, 40, 700, 700, $true) | Out-Null
        }
        if ($FixedWaitSeconds -gt 0) { Start-Sleep -Seconds $FixedWaitSeconds } else { WaitIdle $process }
        [InspectorCheck]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 500

        Click (Descendant $app 'Open menu');           $trace += "click Open menu:      $(State $app)"
        Press 0x28;                                     $trace += "Down:                 $(State $app)"
        if ($inspector) { Click $inspector } else { Start-Sleep -Milliseconds 700 }
        [InspectorCheck]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 700
        $trace += "focus left and back:  $(State $app)"
        Press 0x28;                                     $trace += "Down again:           $(State $app)"
        Press 0x0D;                                     $trace += "Enter:                $(State $app)"
        Click (Descendant $app 'Name')
        foreach ($vk in 0x41, 0x42, 0x43) { Press $vk }; $trace += "typed abc:            $(State $app)"
        $c = Center (Descendant $app 'Row 3')
        [InspectorCheck]::SetCursorPos($c[0], $c[1]) | Out-Null
        Start-Sleep -Milliseconds 300
        [InspectorCheck]::mouse_event(0x0800, 0, 0, -240, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 800
        $trace += "wheel over the list:  $(State $app)"

        # Emptying a required field is invalid, but only shown once the field
        # is blurred. Focus moving to the inspector must not count.
        Click ($app.FindFirst('Descendants', (New-Object System.Windows.Automation.PropertyCondition(
            $AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit))))
        foreach ($vk in 0x08, 0x08, 0x08) { Press $vk }
        if ($inspector) { Click $inspector } else { Start-Sleep -Milliseconds 700 }
        [InspectorCheck]::SetForegroundWindow($h) | Out-Null
        Start-Sleep -Milliseconds 700
        $trace += "emptied, focus away:  border=$(Border $app) $(State $app)"
    }
    finally {
        if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    }
    $trace
}

$plain = Run '--plain' $false
$inspected = Run '' $true

if ($plain.Count -ne 8 -or $inspected.Count -ne 8) {
    "expected 8 recorded steps per run, got $($plain.Count) and $($inspected.Count)"
    exit 1
}
$failures = 0
for ($i = 0; $i -lt $plain.Count; $i++) {
    if ($plain[$i] -eq $inspected[$i]) { "ok    $($plain[$i])" }
    else { "FAIL  plain:     $($plain[$i])"; "      inspected: $($inspected[$i])"; $failures++ }
}
if ($failures -gt 0) { "$failures step(s) differ"; exit 1 }
"the inspector changed nothing"
exit 0
