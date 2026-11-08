# Build the OpenArchive MSI installer
# Usage: .\build-msi.ps1
# Output: target\wix\openarchive-<version>-x86_64.msi

$WixTools = "$PSScriptRoot\wix314-tools"

Write-Host "Building release binaries..." -ForegroundColor Cyan
cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "Building MSI installer..." -ForegroundColor Cyan
Set-Location $PSScriptRoot
cargo wix --bin-path $WixTools --no-build --nocapture -L -sval
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "MSI ready:" -ForegroundColor Green
Get-ChildItem "$PSScriptRoot\target\wix\*.msi" | Select-Object FullName, @{N='Size';E={"{0:N0} KB" -f ($_.Length / 1KB)}}
