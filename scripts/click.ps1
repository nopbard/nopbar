# Simulated clicks on the dock for testing. Prints the foreground process
# after each click. Usage: click.ps1 -X 1280 -Y 1515 [-Count 3] [-Button left|right|middle]
param(
    [Parameter(Mandatory)] [int]$X,
    [Parameter(Mandatory)] [int]$Y,
    [int]$Count = 1,
    [ValidateSet("left", "right", "middle")] [string]$Button = "left"
)
Add-Type -AssemblyName System.Windows.Forms
if (-not ("DevInput" -as [type])) {
    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class DevInput {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, IntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public static string Fg() {
    uint p; var h = GetForegroundWindow(); GetWindowThreadProcessId(h, out p);
    try { return System.Diagnostics.Process.GetProcessById((int)p).ProcessName + " " + h; } catch { return "? " + h; }
  }
}
'@
}
[DevInput]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$flags = @{ left = @(2, 4); right = @(8, 16); middle = @(32, 64) }[$Button]
$orig = [System.Windows.Forms.Cursor]::Position
Write-Output "before: fg=$([DevInput]::Fg())"
for ($i = 1; $i -le $Count; $i++) {
    [DevInput]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 350
    [DevInput]::mouse_event($flags[0], 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [DevInput]::mouse_event($flags[1], 0, 0, 0, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 800
    Write-Output "click $i : fg=$([DevInput]::Fg())"
}
if ($Button -ne "right") { [DevInput]::SetCursorPos($orig.X, $orig.Y) | Out-Null }
