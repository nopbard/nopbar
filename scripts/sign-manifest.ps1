# Signs an update manifest (docs/updates.md, "Trust"):
#   .\scripts\sign-manifest.ps1 site\nopbar\version.json
#   .\scripts\sign-manifest.ps1 D:\test\version.json -KeyFile D:\safe\nopbar.key
# Writes <manifest>.sig: 128 hex digits, the ECDSA P-256 signature (r || s)
# over SHA-256 of the file's exact bytes. Upload both files together, next
# to each other (the app fetches <manifest url>.sig). Any later edit of
# version.json, even whitespace or line endings, needs a new signature.
# The key comes from new-signing-key.ps1 and stays outside the repo.
param(
    [Parameter(Mandatory = $true)][string]$Manifest,
    [string]$KeyFile = (Join-Path $env:USERPROFILE ".nopbar\update-signing.key"),
    # Optional: the UPDATE_PUBLIC_KEY in the app, to check the key matches.
    [string]$PublicKey = ""
)
$ErrorActionPreference = "Stop"

if (-not (Test-Path $KeyFile)) { throw "no signing key at $KeyFile (make one with scripts\new-signing-key.ps1)" }
$manifestPath = (Resolve-Path $Manifest).Path
$bytes = [System.IO.File]::ReadAllBytes($manifestPath)

# The manifest must at least be JSON with a version (the app checks the rest).
$text = [System.Text.Encoding]::UTF8.GetString($bytes)
$json = $text.TrimStart([char]0xFEFF) | ConvertFrom-Json
if (-not $json.version) { throw "$manifestPath has no `"version`"" }

$blob = [Convert]::FromBase64String(([System.IO.File]::ReadAllText($KeyFile)).Trim())
$key = [System.Security.Cryptography.CngKey]::Import($blob, [System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
try {
    $ecdsa = New-Object System.Security.Cryptography.ECDsaCng($key)
    $ecdsa.HashAlgorithm = [System.Security.Cryptography.CngAlgorithm]::Sha256
    $sig = $ecdsa.SignData($bytes)
    if ($sig.Length -ne 64) { throw "unexpected signature length $($sig.Length)" }
    if (-not $ecdsa.VerifyData($bytes, $sig)) { throw "the signature does not verify" }
    $public = $key.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
    $publicHex = -join ($public[8..71] | ForEach-Object { $_.ToString("x2") })
} finally {
    $key.Dispose()
}
if ($PublicKey -and ($PublicKey.Trim().ToLowerInvariant() -ne $publicHex)) {
    throw "this key is not the one the app trusts (its public key is $publicHex)"
}

$sigHex = -join ($sig | ForEach-Object { $_.ToString("x2") })
$out = "$manifestPath.sig"
[System.IO.File]::WriteAllText($out, $sigHex + "`n", (New-Object System.Text.UTF8Encoding($false)))
"Signed $($json.version): $out"
"Public key of this signing key: $publicHex"
