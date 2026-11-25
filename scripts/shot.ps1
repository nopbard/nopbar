# Screenshot of the bottom strip of the primary screen (physical pixels).
# Moves the cursor out of the way first unless -KeepCursor is given.
param(
    [Parameter(Mandatory)] [string]$Out,
    [int]$Height = 220,
    [switch]$KeepCursor
)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
if (-not ("DevUser32" -as [type])) {
    Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class DevUser32 {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
}
'@
}
[DevUser32]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$orig = [System.Windows.Forms.Cursor]::Position
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
if (-not $KeepCursor) {
    [DevUser32]::SetCursorPos([int]($b.Width / 2), [int]($b.Height / 3)) | Out-Null
    Start-Sleep -Milliseconds 700
}
$bmp = New-Object System.Drawing.Bitmap $b.Width, $Height
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.X, $b.Bottom - $Height, 0, 0, $bmp.Size)
$bmp.Save($Out)
if (-not $KeepCursor) { [DevUser32]::SetCursorPos($orig.X, $orig.Y) | Out-Null }
Write-Output "saved $Out"
