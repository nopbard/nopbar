# Regenerates THIRD_PARTY_NOTICES.md from Cargo.lock (no network needed).
#   .\scripts\notices.ps1            # writes THIRD_PARTY_NOTICES.md
#   .\scripts\notices.ps1 -Check     # fails if the file is out of date (CI)
# Lists the runtime crates in the Windows release binaries, plus the
# remaining locked dependencies and the bundled assets. The registry cache
# must contain the locked sources (cargo fetch --locked).
param([switch]$Check)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = "x86_64-pc-windows-msvc"

$json = & cargo metadata --format-version 1 --offline --locked --filter-platform $target --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed (run 'cargo fetch' once if the registry cache is empty)" }
$meta = ($json -join "`n") | ConvertFrom-Json
$lockJson = & cargo metadata --format-version 1 --offline --locked --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed (run 'cargo fetch --locked' to cache all locked crates)" }
$lockMeta = ($lockJson -join "`n") | ConvertFrom-Json

$pkgs = @{}
foreach ($p in $meta.packages) { $pkgs[$p.id] = $p }
$nodes = @{}
foreach ($n in $meta.resolve.nodes) { $nodes[$n.id] = $n }
$workspace = @{}
foreach ($id in $meta.workspace_members) { $workspace[$id] = $true }
$releaseNames = @("dock", "nopbar-mcp", "nopbar-setup")
$releaseRoots = @($meta.packages | Where-Object { $_.name -in $releaseNames -and $workspace.ContainsKey($_.id) })
if ($releaseRoots.Count -ne $releaseNames.Count) { throw "missing a release package in the workspace" }

# Walk normal dependency edges and stop at proc macros, which run only at
# compile time. This inventory is a union of the three executables.
$shipped = @{}
$queue = New-Object System.Collections.Queue
foreach ($p in $releaseRoots) { $queue.Enqueue($p.id) }
while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    if ($shipped.ContainsKey($id) -or @($pkgs[$id].targets | Where-Object { $_.kind -contains "proc-macro" }).Count -gt 0) { continue }
    $shipped[$id] = $true
    foreach ($d in $nodes[$id].deps) {
        if (@($d.dep_kinds | ForEach-Object { $_.kind }) -contains $null) { $queue.Enqueue($d.pkg) }
    }
}
$other = @{}
foreach ($p in $lockMeta.packages) {
    if (-not $workspace.ContainsKey($p.id) -and -not $shipped.ContainsKey($p.id)) {
        $pkgs[$p.id] = $p
        $other[$p.id] = $true
    }
}

function OrderedPackages($ids) {
    # Sort identically under Windows PowerShell and PowerShell 7.
    $ordered = [System.Collections.Generic.SortedDictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($id in $ids.Keys) {
        if ($workspace.ContainsKey($id)) { continue }
        $p = $pkgs[$id]
        $ordered.Add("$($p.name) $($p.version) $id", $p)
    }
    $ordered.Values
}

function Rows($ids) {
    OrderedPackages $ids | ForEach-Object {
        $lic = if ($_.license) { $_.license } elseif ($_.license_file) { "see $($_.license_file)" } else { "unknown" }
        $repo = if ($_.repository) { $_.repository } elseif ($_.homepage) { $_.homepage } else { "https://crates.io/crates/$($_.name)" }
        "| $($_.name) | $($_.version) | $lic | <$repo> |"
    }
}

$shippedRows = @(Rows $shipped)
$otherRows = @(Rows $other)


# ---- License texts of the shipped crates
#
# Read from each crate's source folder (cargo metadata's manifest_path):
# LICENSE*, LICENCE*, COPYING*, UNLICENSE*, NOTICE*, COPYRIGHT*, plus the
# crate's `license-file`. Crates with identical texts share one block. A
# crate that ships no license file gets the standard text of its license
# (from another crate here, or the MIT template below) with its authors as
# the copyright holder.
$fence = '`' * 3
$mitTemplate = @(
    'Permission is hereby granted, free of charge, to any person obtaining a copy'
    'of this software and associated documentation files (the "Software"), to deal'
    'in the Software without restriction, including without limitation the rights'
    'to use, copy, modify, merge, publish, distribute, sublicense, and/or sell'
    'copies of the Software, and to permit persons to whom the Software is'
    'furnished to do so, subject to the following conditions:'
    ''
    'The above copyright notice and this permission notice shall be included in all'
    'copies or substantial portions of the Software.'
    ''
    'THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR'
    'IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,'
    'FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE'
    'AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER'
    'LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,'
    'OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE'
    'SOFTWARE.'
) -join "`n"

function Norm([string]$t) {
    $ls = ($t -replace "`r`n", "`n" -replace "`r", "`n").Split("`n") | ForEach-Object {
        $line = $_.TrimEnd()
        # Normalize a decorative divider between notices.
        if ($line -eq [string][char]0x2014) { "---" } else { $line }
    }
    # A fence inside a text would end the code block early.
    (($ls -join "`n").Trim("`n")).Replace($fence, "'''")
}
function Authors($p) {
    $a = @($p.authors | Where-Object { $_ })
    if ($a.Count) { ($a | ForEach-Object { $_ -replace '\s*<[^>]*>', '' }) -join ", " } else { "the $($p.name) authors" }
}

$shippedPkgs = @(OrderedPackages $shipped)
$byId = @{}   # license id (lowercase) -> a full text seen in some crate
$found = @{}  # package id -> texts @{ name; text }
foreach ($p in $shippedPkgs) {
    $dir = Split-Path -Parent $p.manifest_path
    $files = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(licen[cs]e|copying|unlicense|notice|copyright)' } | Sort-Object Name)
    if ($p.license_file) {
        $lf = Join-Path $dir $p.license_file
        if ((Test-Path -LiteralPath $lf) -and -not ($files | Where-Object { $_.FullName -eq (Get-Item -LiteralPath $lf).FullName })) { $files += Get-Item -LiteralPath $lf }
    }
    $list = @($files | ForEach-Object { @{ name = $_.Name; text = (Norm ([System.IO.File]::ReadAllText($_.FullName))) } })
    foreach ($f in $list) {
        $n = $f.name.ToLowerInvariant()
        $k = if ($n -match 'apache') { "apache-2.0" } elseif ($n -match 'unlicense') { "unlicense" } elseif ($n -match 'mit') { "mit" } else { $null }
        if ($k -and -not $byId.ContainsKey($k)) { $byId[$k] = $f.text }
    }
    $found[$p.id] = $list
}

$groups = New-Object System.Collections.ArrayList
$groupOf = @{}
foreach ($p in $shippedPkgs) {
    $list = $found[$p.id]
    $note = ""
    if ($list.Count -eq 0) {
        $note = " (ships no license file: standard text, copyright $(Authors $p))"
        $ids = @([regex]::Matches([string]$p.license, '[A-Za-z0-9.\-]+') | ForEach-Object { $_.Value } | Where-Object { $_ -notin @("OR", "AND", "WITH") })
        if ($ids.Count -eq 0) { throw "$($p.name): no license field and no license file" }
        $list = @($ids | ForEach-Object {
            $k = $_.ToLowerInvariant()
            $t = if ($byId.ContainsKey($k)) { $byId[$k] } elseif ($k -eq "mit") { $mitTemplate } else { throw "$($p.name): no license file and no standard text for $_" }
            @{ name = $_; text = "Copyright (c) $(Authors $p)`n`n" + $t }
        })
    }
    $key = ($list | ForEach-Object { $_.name.ToLowerInvariant() + "`n" + $_.text }) -join "`n---`n"
    if (-not $groupOf.ContainsKey($key)) {
        $groupOf[$key] = [pscustomobject]@{ texts = $list; names = (New-Object System.Collections.ArrayList); members = (New-Object System.Collections.ArrayList) }
        [void]$groups.Add($groupOf[$key])
    }
    $g = $groupOf[$key]
    if ($g.names -notcontains $p.name) { [void]$g.names.Add($p.name) }
    [void]$g.members.Add("- $($p.name) $($p.version) ($($p.license)), by $(Authors $p)$note")
}

$textLines = @()
foreach ($g in $groups) {
    $textLines += "### " + ($g.names -join ", ")
    $textLines += ""
    $textLines += $g.members
    foreach ($t in $g.texts) {
        $textLines += ""
        $textLines += "**$($t.name)**"
        $textLines += ""
        $textLines += ($fence + "text")
        $textLines += $t.text
        $textLines += $fence
    }
    $textLines += ""
}
# The font built into nopbar.exe (fonts.rs: include_bytes!): its license
# text as shipped with it (dock/assets/fonts, the release zip's LICENSE.txt).
$ofl = Join-Path $root "dock\assets\fonts\LICENSE-Pretendard.txt"
if (-not (Test-Path $ofl)) { throw "missing dock\assets\fonts\LICENSE-Pretendard.txt" }
$oflText = (([System.IO.File]::ReadAllText($ofl)) -replace "`r`n", "`n").TrimEnd()
$stdApache = if ($byId.ContainsKey("apache-2.0")) { "the Apache-2.0 text is the same as the crates' above" } else { "Apache-2.0: https://www.apache.org/licenses/LICENSE-2.0" }

$lines = @(
    "# Third-party notices"
    ""
    "<!-- Generated by scripts/notices.ps1 from Cargo.lock. Do not edit by hand. -->"
    ""
    "NopBar source code is licensed under [GPL-3.0-only](LICENSE). The"
    "components below retain their own licenses. The license texts and"
    "copyright notices for the release dependencies follow the lists."
    ""
    "## Rust crates in the Windows release binaries"
    ""
    "Normal dependencies of nopbar.exe, nopbar-mcp.exe, and the setup launcher"
    "on $target ($($shippedRows.Count) crates)."
    ""
    "| Crate | Version | License | Source |"
    "|---|---|---|---|"
) + $shippedRows + @(
    ""
    "## Rust toolchain"
    ""
    "| Component | License | Source |"
    "|---|---|---|"
    "| Rust standard library (linked into nopbar.exe) | MIT OR Apache-2.0 | <https://github.com/rust-lang/rust> |"
    ""
    "## Fonts in nopbar.exe"
    ""
    "| Font | Version | License | Source | Notes |"
    "|---|---|---|---|---|"
    "| Pretendard (variable, ``PretendardVariable.ttf``) by Kil Hyung-jin | 1.3.9 | SIL Open Font License 1.1 (Reserved Font Name Pretendard) | <https://github.com/orioncactus/pretendard> | The official release file, unmodified, built into the exe (``dock/assets/fonts/``) and read from memory; never installed on the PC. The default UI font. |"
    ""
    "## Other locked Rust dependencies"
    ""
    "Build, test, and other target dependencies outside the Windows release"
    "inventory ($($otherRows.Count) crates)."
    ""
    "| Crate | Version | License | Source |"
    "|---|---|---|---|"
) + $otherRows + @(
    ""
    "## Third-party marks"
    ""
    "The app includes the OpenAI, Anthropic, and Google provider marks under"
    "assets/brands/. The [asset sources and brand terms](assets/brands/README.md)"
    "identify each mark and its owner."
    ""
    "## NopBar design assets and trademarks"
    ""
    "See [assets/LICENSE](assets/LICENSE) and [TRADEMARKS.md](TRADEMARKS.md)"
    "for the reserved NopBar artwork, name, logo, and app icon."
    ""
    "## License texts"
    ""
    "As shipped in each crate's source package on crates.io, grouped where the"
    "texts are identical. Authors are taken from each crate's Cargo.toml."
    ""
) + $textLines + @(
    "### Rust standard library"
    ""
    "- std, core, alloc and the crates they use (MIT OR Apache-2.0), by The Rust"
    "  Project Contributors (<https://github.com/rust-lang/rust/blob/master/COPYRIGHT>)"
    ""
    "**MIT** ($stdApache)"
    ""
    ($fence + "text")
    ("Copyright (c) The Rust Project Contributors`n`n" + $mitTemplate)
    $fence
    ""
    "### Pretendard"
    ""
    "- PretendardVariable.ttf 1.3.9, by Kil Hyung-jin (<https://github.com/orioncactus/pretendard>)"
    ""
    "**SIL Open Font License 1.1**"
    ""
    ($fence + "text")
    $oflText
    $fence
)
$text = ($lines -join "`n") + "`n"
$out = Join-Path $root "THIRD_PARTY_NOTICES.md"

if ($Check) {
    $old = if (Test-Path $out) { ([System.IO.File]::ReadAllText($out)) -replace "`r`n", "`n" } else { "" }
    if ($old -ne $text) { throw "THIRD_PARTY_NOTICES.md is out of date: run scripts\notices.ps1" }
    "THIRD_PARTY_NOTICES.md is up to date ($($shippedRows.Count) shipped crates)."
    return
}
[System.IO.File]::WriteAllText($out, $text, (New-Object System.Text.UTF8Encoding($false)))
"Wrote $out ($($shippedRows.Count) shipped crates, $($otherRows.Count) other locked, $($groups.Count) license text groups)."
