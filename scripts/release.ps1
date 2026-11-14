# Builds Monaged's release files into dist\:
#   Monaged-Setup-<version>.exe   the installer (the app itself, named so it installs)
#   monaged-<version>-windows-x64.zip
#   SHA256SUMS.txt
# With -Publish it also creates the GitHub release v<version> (on the private
# repo) that installed copies update from. Without it nothing leaves this PC.
param([switch]$Publish, [string]$Repo = "Marten-Mrfc/monaged")
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
Write-Host "Building Monaged $version (release)…"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
New-Item -ItemType Directory -Force dist | Out-Null
Remove-Item dist\* -Force -ErrorAction SilentlyContinue
Copy-Item target\release\monaged.exe "dist\Monaged-Setup-$version.exe"
Compress-Archive -Force -Path target\release\monaged.exe -DestinationPath "dist\monaged-$version-windows-x64.zip"
Get-ChildItem dist -File | Where-Object Name -ne "SHA256SUMS.txt" | ForEach-Object {
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name
} | Set-Content dist\SHA256SUMS.txt
Write-Host "Built:"; Get-ChildItem dist | Format-Table Name, Length
if ($Publish) {
    $notes = if (Test-Path RELEASE_NOTES.md) { "--notes-file=RELEASE_NOTES.md" } else { "--generate-notes" }
    gh release create "v$version" (Get-ChildItem dist -File).FullName --repo $Repo --title "Monaged $version" $notes
    if ($LASTEXITCODE -ne 0) { throw "gh release create failed" }
    Write-Host "Published v$version on $Repo."
}
