# Builds Monaged's release files into dist\ on this PC (see scripts/package.ps1):
#   Monaged-Setup-<version>.exe, monaged-<version>-windows-x64.zip, SHA256SUMS.txt
# With -Publish it also creates the GitHub release v<version> that installed
# copies update from. Usually you don't need this: pushing the tag v<version>
# makes the Release workflow on GitHub build and publish the same files.
param([switch]$Publish, [string]$Repo = "Marten-Mrfc/monaged")
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
Write-Host "Building Monaged $version (release)…"
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
& "$PSScriptRoot\package.ps1" -Version $version
if ($Publish) {
    & "$PSScriptRoot\release-notes.ps1" -Version $version -Out dist-notes.md
    gh release create "v$version" (Get-ChildItem dist -File).FullName --repo $Repo --title "Monaged $version" --notes-file dist-notes.md
    if ($LASTEXITCODE -ne 0) { throw "gh release create failed" }
    Remove-Item dist-notes.md
    Write-Host "Published v$version on $Repo."
}
