# Packs a release build (target\release\monaged.exe) into dist\:
#   Monaged-Setup-<version>.exe      the installer (the app itself, named so it installs)
#   monaged-<version>-windows-x64.zip  the portable app with its readme and license
#   SHA256SUMS.txt
# Used by scripts/release.ps1 and the Release workflow.
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
$exe = "target\release\monaged.exe"
if (-not (Test-Path $exe)) { throw "$exe is missing; run cargo build --release first" }
# build.rs embeds the icon and version info when the Windows SDK's rc.exe is there.
$info = (Get-Item $exe).VersionInfo
if ($info.ProductVersion -ne $Version) {
    Write-Warning "monaged.exe has no version info for $Version (rc.exe missing?), so it has no icon either."
} else {
    Write-Host "monaged.exe: $($info.ProductName) $($info.ProductVersion), with its icon"
}
New-Item -ItemType Directory -Force dist | Out-Null
Remove-Item dist\* -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item $exe "dist\Monaged-Setup-$Version.exe"
$portable = Join-Path ([IO.Path]::GetTempPath()) "monaged-portable-$Version"
Remove-Item $portable -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $portable | Out-Null
Copy-Item $exe, README.md, LICENSE, CHANGELOG.md $portable
Compress-Archive -Force -Path "$portable\*" -DestinationPath "dist\monaged-$Version-windows-x64.zip"
Remove-Item $portable -Recurse -Force
Get-ChildItem dist -File | Where-Object Name -ne "SHA256SUMS.txt" | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
} | Set-Content -Encoding ascii dist\SHA256SUMS.txt
Get-ChildItem dist | Format-Table Name, Length
