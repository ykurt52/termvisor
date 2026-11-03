# Builds the installer from the folder made by bundle.sh, with Inno Setup.
#
#   setup.ps1 <version> <bundle-dir> <out-dir>
#
# Writes <out-dir>/termvisor-windows-x64_v<version>_setup.exe.

param(
    [Parameter(Mandatory)] [string] $Version,
    [Parameter(Mandatory)] [string] $Bundle,
    [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Stop'

$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
if (-not (Test-Path $iscc)) { throw "Inno Setup not found" }

$script = Join-Path $PSScriptRoot 'termvisor.iss'
& $iscc /Qp "/DAppVersion=$Version" "/DSource=$((Resolve-Path $Bundle).Path)" "/O$((Resolve-Path $Out).Path)" $script
if ($LASTEXITCODE -ne 0) { throw "ISCC failed with $LASTEXITCODE" }
Get-Item (Join-Path $Out "termvisor-windows-x64_v${Version}_setup.exe")
