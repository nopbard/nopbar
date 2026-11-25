# Rebuilds the dock, restarts it from target\run (so cargo can overwrite
# target\debug while it runs) and optionally takes a screenshot of the
# bottom of the primary screen.
param(
    [string]$Shot = "",
    [int]$ShotHeight = 220
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Get-Process dock -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800

cargo build -q
if ($LASTEXITCODE -ne 0) { Write-Output "BUILD FAILED"; exit 1 }

New-Item -ItemType Directory -Force target\run | Out-Null
Copy-Item target\debug\dock.exe target\run\dock.exe -Force
Start-Process target\run\dock.exe
Start-Sleep -Seconds 3
Write-Output "dock running: $((Get-Process dock -ErrorAction SilentlyContinue | Measure-Object).Count) process(es)"

if ($Shot) {
    & "$PSScriptRoot\shot.ps1" -Out $Shot -Height $ShotHeight
}
