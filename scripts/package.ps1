# Builds release binaries and stages the packages in dist\ (docs/release.md).
#   .\scripts\package.ps1 -Installer      # direct-download setup (setup window around Inno Setup) -> dist\installer
#   .\scripts\package.ps1 -Zip            # portable zip                       -> dist\zip
# Switches combine (one build). Signing (signtool, Windows SDK) is on when
# -Sign is given or any NOPBAR_SIGN_* variable is set:
#   NOPBAR_SIGN_THUMBPRINT   SHA-1 of a cert in CurrentUser\My (or -CertThumbprint)
#   NOPBAR_SIGN_PFX          a .pfx file, with NOPBAR_SIGN_PFX_PASSWORD
#   NOPBAR_SIGN_DLIB         Azure Artifact (Trusted) Signing dlib, with NOPBAR_SIGN_DMDF (metadata.json)
#   NOPBAR_SIGN_TIMESTAMP    RFC 3161 timestamp URL (default http://timestamp.digicert.com)
# Unsigned otherwise (SmartScreen warns on first downloads; see docs/release.md).
param(
    [switch]$Installer,
    [switch]$Zip,
    [switch]$Sign,
    [string]$CertThumbprint = ""
)
$ErrorActionPreference = "Stop"
if (-not ($Installer -or $Zip)) { throw "nothing to make: pass -Installer and/or -Zip" }
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The app version (dock/Cargo.toml), for the installer and zip names.
$appVersion = (Select-String -Path (Join-Path $root "dock\Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $appVersion) { throw "no version in dock\Cargo.toml" }

# ---- signing settings
$signThumb = if ($CertThumbprint) { $CertThumbprint } else { $env:NOPBAR_SIGN_THUMBPRINT }
$signPfx = $env:NOPBAR_SIGN_PFX
$signDlib = $env:NOPBAR_SIGN_DLIB
$timestamp = if ($env:NOPBAR_SIGN_TIMESTAMP) { $env:NOPBAR_SIGN_TIMESTAMP } else { "http://timestamp.digicert.com" }
$doSign = $Sign -or [bool]$signThumb -or [bool]$signPfx -or [bool]$signDlib

function Find-SdkTool([string]$name) {
    $kits = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (-not (Test-Path $kits)) { return $null }
    Get-ChildItem $kits -Recurse -Filter $name -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } |
        Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
}

# Inno Setup's command-line compiler: PATH, then the usual install folders
# (machine-wide, or per user as winget installs it without admin).
function Find-Iscc {
    $cmd = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($dir in "${env:ProgramFiles(x86)}\Inno Setup 6", "$env:ProgramFiles\Inno Setup 6", "$env:LOCALAPPDATA\Programs\Inno Setup 6") {
        $p = Join-Path $dir "ISCC.exe"
        if (Test-Path $p) { return $p }
    }
    return $null
}

# ---- check inputs and tools before the (slow) build
$iscc = $null
if ($Installer) {
    $iscc = Find-Iscc
    if (-not $iscc) {
        Write-Host "Inno Setup 6 (ISCC.exe) not found. Install it, then run this again:" -ForegroundColor Yellow
        Write-Host "    winget install --id JRSoftware.InnoSetup -e" -ForegroundColor Yellow
        Write-Host "  (or https://jrsoftware.org/isdl.php; 6.7 or newer)" -ForegroundColor Yellow
        throw "ISCC.exe not found"
    }
    # nopbar.iss uses the 6.7 wizard look (WizardBackColor, hidebevels). ISCC.exe
    # has no version resource: a tiny script asks ISPP (no output is made).
    $probe = "#if VER < EncodeVer(6,7,0)`r`n#error old`r`n#endif`r`n[Setup]`r`nAppName=x`r`nAppVersion=1`r`nDefaultDirName=x`r`n"
    $probe | & $iscc /Q /O- - | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "$iscc is older than Inno Setup 6.7. Update it, then run this again:" -ForegroundColor Yellow
        Write-Host "    winget upgrade --id JRSoftware.InnoSetup -e" -ForegroundColor Yellow
        throw "Inno Setup 6.7 or newer needed"
    }
    # Languages nopbar.iss would use but this Inno Setup lacks (skipped, not an error).
    $langDir = Join-Path (Split-Path -Parent $iscc) "Languages"
    $wanted = Select-String -Path (Join-Path $root "packaging\inno\nopbar.iss") -Pattern '^\{#Lang\("[^"]+", "([^"]+)"\)\}' |
        ForEach-Object { $_.Matches[0].Groups[1].Value }
    $missing = $wanted | Where-Object { -not (Test-Path (Join-Path $langDir "$_.isl")) }
    if ($missing) {
        Write-Host "Installer languages skipped (no .isl in $langDir; see docs/release.md): $($missing -join ', ')" -ForegroundColor Yellow
    }
}
$signtool = $null
if ($doSign) {
    if (-not ($signThumb -or $signPfx -or $signDlib)) { throw "-Sign needs -CertThumbprint or a NOPBAR_SIGN_* variable (see the top of this script)" }
    if ($signPfx -and -not (Test-Path $signPfx)) { throw "NOPBAR_SIGN_PFX: $signPfx not found" }
    if ($signDlib -and -not $env:NOPBAR_SIGN_DMDF) { throw "NOPBAR_SIGN_DLIB needs NOPBAR_SIGN_DMDF (metadata.json)" }
    $signtool = Find-SdkTool "signtool.exe"
    if (-not $signtool) { throw "signtool.exe not found (install the Windows SDK)" }
}

# Removes an old output folder; anything that cannot be removed stops the build
# (stale files must never end up in a package).
function Reset-Dir([string]$dir) {
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force -ErrorAction Stop }
    New-Item -ItemType Directory -Force $dir | Out-Null
}

function Sign-File([string]$path) {
    if (-not $doSign) { return }
    $a = @("sign", "/fd", "SHA256", "/tr", $timestamp, "/td", "SHA256")
    if ($signDlib) { $a += @("/dlib", $signDlib, "/dmdf", $env:NOPBAR_SIGN_DMDF) }
    elseif ($signPfx) {
        $a += @("/f", $signPfx)
        if ($env:NOPBAR_SIGN_PFX_PASSWORD) { $a += @("/p", $env:NOPBAR_SIGN_PFX_PASSWORD) }
    }
    else { $a += @("/sha1", $signThumb) }
    & $signtool @a $path
    if ($LASTEXITCODE -ne 0) { throw "signing $path failed" }
}

function Show-Hash([string]$path) {
    "  SHA256 $((Get-FileHash $path -Algorithm SHA256).Hash)  $((Get-Item $path).Length) bytes  $(Split-Path -Leaf $path)"
}

# The version.json "downloads" entry for a file (docs/updates.md): the app
# installs automatic updates from "zip" and needs all three fields.
function Show-Manifest([string]$kind, [string]$path) {
    $sha = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant()
    $size = (Get-Item $path).Length
    "  version.json: `"$kind`": { `"file`": `"$(Split-Path -Leaf $path)`", `"sha256`": `"$sha`", `"size`": $size }"
}

cargo build --release -p dock -p nopbar-mcp
if ($LASTEXITCODE -ne 0) { throw "build failed" }
$bin = Join-Path $root "target\release"
# The exes must start without the Visual C++ runtime (.cargo\config.toml,
# static CRT): an import of VCRUNTIME140.dll means a clean Windows cannot run
# them (the setup failed with STATUS_DLL_NOT_FOUND in Windows Sandbox).
function Assert-StaticCrt([string]$exe) {
    $text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($exe))
    if ($text -match '(?i)vcruntime\d+\.dll|msvcp\d+\.dll') { throw "$exe imports the Visual C++ runtime ($($Matches[0])): build from the repo folder so .cargo\config.toml applies" }
}
Assert-StaticCrt "$bin\nopbar.exe"
Assert-StaticCrt "$bin\nopbar-mcp.exe"
# Pretendard, the default UI font, is built into nopbar.exe (dock/src/fonts.rs,
# include_bytes!): the installer, the zip and automatic updates carry it with
# the exe, and its license (SIL OFL 1.1) is in THIRD_PARTY_NOTICES.md, so in
# LICENSE.txt below. An exe smaller than the font cannot be carrying it.
$font = Join-Path $root "dock\assets\fonts\PretendardVariable.ttf"
if ((Get-Item "$bin\nopbar.exe").Length -le (Get-Item $font).Length) { throw "nopbar.exe does not carry $font" }

# The installer and the portable zip carry the same (signed) nopbar.exe.
if ($Installer -or $Zip) {
    $stage = Join-Path $root "dist\direct"
    Reset-Dir $stage
    Copy-Item "$bin\nopbar.exe" $stage
    Copy-Item "$bin\nopbar-mcp.exe" $stage
    # One LICENSE.txt carries every notice that must travel with the exe:
    # NopBar's GPL-3.0 license, the terms of the reserved assets (icon) and the
    # third-party license texts. (release::INSTALLED_FILES lists LICENSE.txt,
    # so the uninstaller needs no change.)
    $sep = "`r`n`r`n" + ("=" * 78) + "`r`n`r`n"
    $parts = foreach ($f in "LICENSE", "assets\LICENSE", "THIRD_PARTY_NOTICES.md") {
        $p = Join-Path $root $f
        if (-not (Test-Path $p)) { throw "missing $f" }
        ([System.IO.File]::ReadAllText($p)) -replace "`r?`n", "`r`n"
    }
    [System.IO.File]::WriteAllText((Join-Path $stage "LICENSE.txt"), ($parts -join $sep), (New-Object System.Text.UTF8Encoding($true)))
    Sign-File "$stage\nopbar.exe"
    Sign-File "$stage\nopbar-mcp.exe"
}

if ($Installer) {
    # Two steps. 1) Inno Setup builds the real installer as
    # NopBar-<v>-inner.exe (/F overrides OutputBaseFilename; nopbar.iss is
    # unchanged), signed. 2) The setup window (setup\, nopbar-setup.exe)
    # embeds it and becomes NopBar-<v>-setup.exe, signed: what people
    # download and what installed copies update with (with Inno switches
    # it runs the inner installer as is; setup\src\cli.rs).
    $innerDir = Join-Path $root "dist\inner"
    Reset-Dir $innerDir
    & $iscc /Q "/DAppVersion=$appVersion" "/DSourceExe=$stage\nopbar.exe" "/DSourceMcp=$stage\nopbar-mcp.exe" "/DLicenseFile=$stage\LICENSE.txt" "/DOutputDir=$innerDir" "/FNopBar-$appVersion-inner" (Join-Path $root "packaging\inno\nopbar.iss")
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
    $inner = Join-Path $innerDir "NopBar-$appVersion-inner.exe"
    if (-not (Test-Path $inner)) { throw "ISCC made no $inner" }
    Sign-File $inner
    $env:NOPBAR_SETUP_INNER = $inner
    try {
        cargo build --release -p nopbar-setup
        if ($LASTEXITCODE -ne 0) { throw "setup build failed" }
        Assert-StaticCrt "$bin\nopbar-setup.exe"
    }
    finally { Remove-Item Env:\NOPBAR_SETUP_INNER -ErrorAction SilentlyContinue }
    $out = Join-Path $root "dist\installer"
    Reset-Dir $out
    $setup = Join-Path $out "NopBar-$appVersion-setup.exe"
    Copy-Item "$bin\nopbar-setup.exe" $setup
    # The updater checks the setup's ProductVersion against the manifest
    # (dock/src/autoupdate/trust.rs); the embedded installer must be in it.
    $pv = (Get-Item $setup).VersionInfo.ProductVersion
    if ($pv -ne $appVersion) { throw "$setup has ProductVersion '$pv', not $appVersion" }
    $innerSize = (Get-Item $inner).Length
    if ((Get-Item $setup).Length -le $innerSize) { throw "$setup does not carry the installer ($innerSize bytes)" }
    Sign-File $setup
    "Inner installer: $inner"
    Show-Hash $inner
    "Installer: $setup"
    Show-Hash $setup
    Show-Manifest "setup" $setup
}

if ($Zip) {
    $out = Join-Path $root "dist\zip"
    Reset-Dir $out
    $zipFile = Join-Path $out "NopBar-$appVersion-portable.zip"
    Compress-Archive -Path "$stage\nopbar.exe", "$stage\nopbar-mcp.exe", "$stage\LICENSE.txt" -DestinationPath $zipFile
    "Portable zip: $zipFile"
    Show-Hash $zipFile
    Show-Manifest "zip" $zipFile
}

if (-not $doSign) {
    Write-Host "Unsigned: SmartScreen will warn until the download builds reputation (docs/release.md)." -ForegroundColor Yellow
}
