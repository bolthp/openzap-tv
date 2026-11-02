# Downloads libmpv-2.dll (Windows x86_64) into the given directory.
# The build is pinned by release tag and verified against its SHA-256, so
# CI and releases always use exactly the same binary.
#
# Usage: pwsh scripts/fetch-libmpv-windows.ps1 -Dest <dir>
param(
    [Parameter(Mandatory = $true)][string]$Dest
)
$ErrorActionPreference = 'Stop'

# To update: pick a release of https://github.com/shinchiro/mpv-winbuild-cmake,
# then change the three values below (the SHA-256 is shown next to each asset).
$Tag = '20261102'
$Asset = 'mpv-dev-x86_64-20261102-git-2137d21b2e.7z'
$Sha256 = '5ca801552661c18fa0b1edfd41dba6e5d9aa9d6d3d6c42dbe3fb91a45eb79f16'

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$archive = Join-Path $Dest $Asset
Invoke-WebRequest -Uri "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$Tag/$Asset" -OutFile $archive

$actual = (Get-FileHash -Path $archive -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $Sha256) {
    Remove-Item $archive
    throw "SHA-256 mismatch for $Asset`n  expected: $Sha256`n  actual:   $actual"
}

7z x $archive "-o$Dest" libmpv-2.dll -y | Out-Null
if ($LASTEXITCODE -ne 0) { throw "7z failed with exit code $LASTEXITCODE" }
Remove-Item $archive
Write-Host "libmpv-2.dll is in $Dest"
