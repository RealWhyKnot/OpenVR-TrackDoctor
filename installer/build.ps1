#Requires -Version 5.1
[CmdletBinding()]
param(
	[string]$Version
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

if (-not $Version) {
	$Version = (Select-String -LiteralPath (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
}
$nums = @($Version -split '[^0-9]+' | Where-Object { $_ -ne '' } | Select-Object -First 4)
while ($nums.Count -lt 4) { $nums += '0' }
$viversion = $nums -join '.'

cargo build --release --manifest-path (Join-Path $root "Cargo.toml")
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

$out = Join-Path $root "target\installer"
$payload = Join-Path $out "payload"
New-Item -ItemType Directory -Force -Path $payload | Out-Null
foreach ($f in @("target\release\trackdoctor.exe", "target\release\trackdoctor-bg.exe", "README.md", "LICENSE")) {
	Copy-Item -LiteralPath (Join-Path $root $f) -Destination $payload -Force
}

$nsis = Join-Path ${env:ProgramFiles(x86)} "NSIS\makensis.exe"
if (-not (Test-Path -LiteralPath $nsis)) { throw "NSIS not found at $nsis (install it from https://nsis.sourceforge.io)" }
$setup = Join-Path $out "TrackDoctor-Setup-$Version.exe"
& $nsis /V2 /WX "/DVERSION=$Version" "/DVIVERSION=$viversion" "/DPAYLOAD=$payload" "/DOUTFILE=$setup" (Join-Path $PSScriptRoot "installer.nsi")
if ($LASTEXITCODE -ne 0) { throw "makensis failed ($LASTEXITCODE)" }
Write-Host "Built $setup"
