# Writes the release notes for one version: its section of CHANGELOG.md plus
# how to install. Used by scripts/release.ps1 and the Release workflow.
param([Parameter(Mandatory)][string]$Version, [string]$Out = "RELEASE_NOTES.md")
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
$lines = Get-Content CHANGELOG.md
$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match "^## \[?$([regex]::Escape($Version))\]?") { $start = $i + 1; break }
}
$section = @()
if ($start -ge 0) {
    for ($i = $start; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^## ') { break }
        $section += $lines[$i]
    }
}
if ($section.Count -eq 0) { $section = @("Monaged $Version.") }
$install = @"

## Install

1. Download **Monaged-Setup-$Version.exe** below and run it. It installs just for you (no admin) into ``%LOCALAPPDATA%\Programs\Monaged``.
2. Windows SmartScreen may say it protected your PC, because the installer isn't code-signed yet: click **More info → Run anyway**.
3. Monaged opens its setup guide, which installs WSL and Ubuntu if you need them, then the Claude agent, and signs you in.

Already installed? Monaged updates itself: **Settings → General → Check for updates**.

The zip is the same app without installing. ``SHA256SUMS.txt`` lists the checksums.
"@
(($section -join "`n").Trim() + "`n" + $install) | Set-Content -Encoding utf8 $Out
Write-Host "Wrote $Out"
