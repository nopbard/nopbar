# Makes the update-manifest signing key (ECDSA P-256) once (docs/updates.md, "Trust").
#   .\scripts\new-signing-key.ps1                      # -> %USERPROFILE%\.nopbar\update-signing.key
#   .\scripts\new-signing-key.ps1 -KeyFile D:\safe\nopbar.key
# The private key stays OUTSIDE the repo (the script refuses a path inside
# it) and is readable by you only. Back it up somewhere safe: losing it
# means shipping a build with a new public key before updates work again.
# Prints the public key: paste it into UPDATE_PUBLIC_KEY in
# dock/src/autoupdate/trust.rs, then build the release.
param(
    [string]$KeyFile = (Join-Path $env:USERPROFILE ".nopbar\update-signing.key"),
    [switch]$Force
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$full = [System.IO.Path]::GetFullPath($KeyFile)
if ($full.StartsWith([System.IO.Path]::GetFullPath($root).TrimEnd('\') + '\', [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "The key must not live inside the repository ($full). Pick a path outside it."
}
if ((Test-Path $full) -and -not $Force) {
    throw "$full exists. It is the key every released NopBar trusts; use -Force only if you really mean to replace it."
}

$dir = Split-Path -Parent $full
New-Item -ItemType Directory -Force $dir | Out-Null

$params = New-Object System.Security.Cryptography.CngKeyCreationParameters
$params.ExportPolicy = [System.Security.Cryptography.CngExportPolicies]::AllowPlaintextExport
$key = [System.Security.Cryptography.CngKey]::Create([System.Security.Cryptography.CngAlgorithm]::ECDsaP256, $null, $params)
try {
    $private = $key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
    $public = $key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
} finally {
    $key.Dispose()
}
# BCRYPT_ECCKEY_BLOB: magic (4), key size (4), X (32), Y (32)[, d (32)].
if ($public.Length -ne 72) { throw "unexpected public key blob ($($public.Length) bytes)" }
$hex = -join ($public[8..71] | ForEach-Object { $_.ToString("x2") })

[System.IO.File]::WriteAllText($full, [Convert]::ToBase64String($private), (New-Object System.Text.UTF8Encoding($false)))
# You only: no inherited permissions, full control for the current user.
& icacls.exe $full /inheritance:r /grant:r "${env:USERDOMAIN}\${env:USERNAME}:F" | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Host "Could not restrict $full to your account (icacls $LASTEXITCODE); check its permissions." -ForegroundColor Yellow }

"Private key: $full  (never commit it; back it up offline)"
""
"Public key - paste into dock/src/autoupdate/trust.rs:"
"pub const UPDATE_PUBLIC_KEY: &str = `"$hex`";"
