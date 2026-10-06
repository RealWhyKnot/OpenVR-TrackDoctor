#Requires -Version 5.1
[CmdletBinding()]
param(
	[Parameter(Mandatory = $true)][string]$Setup,
	[Parameter(Mandatory = $true)][string]$Version
)

$ErrorActionPreference = "Stop"
$script:failures = @()

function Assert([bool]$Condition, [string]$Message) {
	if ($Condition) {
		Write-Host "ok   $Message"
	}
	else {
		$script:failures += $Message
		Write-Host "FAIL $Message"
	}
}

$dir = Join-Path $env:TEMP "TrackDoctor Smoke"
$arpPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TrackDoctor"
$menu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\TrackDoctor"
$data = Join-Path $env:LOCALAPPDATA "trackdoctor"

if (Test-Path -Path $arpPath) {
	throw "refusing to run: TrackDoctor is already installed on this machine and the test would remove it"
}
if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
$hadData = Test-Path -LiteralPath $data

$install = Start-Process -FilePath $Setup -ArgumentList ("/S /D=" + $dir) -Wait -PassThru
Assert ($install.ExitCode -eq 0) "installer exit code 0 (got $($install.ExitCode))"

foreach ($f in @("trackdoctor.exe", "trackdoctor-bg.exe", "README.md", "LICENSE", "Uninstall.exe")) {
	Assert (Test-Path -LiteralPath (Join-Path $dir $f)) "installed $f"
}
foreach ($l in @("TrackDoctor.lnk", "TrackDoctor last session report.lnk", "TrackDoctor USB layout.lnk", "Recorded sessions.lnk", "Uninstall TrackDoctor.lnk")) {
	Assert (Test-Path -LiteralPath (Join-Path $menu $l)) "start menu $l"
}
Assert (Test-Path -LiteralPath (Join-Path $data "sessions")) "sessions folder created"

$arp = Get-ItemProperty -Path $arpPath
Assert ($arp.DisplayName -eq "TrackDoctor") "DisplayName (got $($arp.DisplayName))"
Assert ($arp.DisplayVersion -eq $Version) "DisplayVersion $Version (got $($arp.DisplayVersion))"
Assert ($arp.Publisher -eq "RealWhyKnot") "Publisher (got $($arp.Publisher))"
Assert ($arp.InstallLocation -eq $dir) "InstallLocation (got $($arp.InstallLocation))"
Assert ($arp.UninstallString -eq ('"' + (Join-Path $dir "Uninstall.exe") + '"')) "UninstallString (got $($arp.UninstallString))"
Assert ($arp.QuietUninstallString -eq ('"' + (Join-Path $dir "Uninstall.exe") + '" /S')) "QuietUninstallString (got $($arp.QuietUninstallString))"
Assert ($arp.EstimatedSize -gt 0) "EstimatedSize (got $($arp.EstimatedSize))"

$help = & (Join-Path $dir "trackdoctor.exe") help
Assert ($LASTEXITCODE -eq 0 -and ($help -join "`n") -match "autostart on") "installed trackdoctor.exe runs"

$uninstall = Start-Process -FilePath (Join-Path $dir "Uninstall.exe") -ArgumentList ("/S _?=" + $dir) -Wait -PassThru
Assert ($uninstall.ExitCode -eq 0) "uninstaller exit code 0 (got $($uninstall.ExitCode))"

Assert (-not (Test-Path -Path $arpPath)) "uninstall entry removed"
Assert (-not (Test-Path -LiteralPath $menu)) "start menu folder removed"
$left = @()
if (Test-Path -LiteralPath $dir) {
	$left = @(Get-ChildItem -LiteralPath $dir -Recurse -Force | Where-Object { $_.Name -ne "Uninstall.exe" })
}
Assert ($left.Count -eq 0) "install dir emptied (left: $(($left | ForEach-Object Name) -join ', '))"
Assert (Test-Path -LiteralPath (Join-Path $data "sessions")) "silent uninstall keeps recorded sessions"
if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
if (-not $hadData -and (Test-Path -LiteralPath $data)) { Remove-Item -LiteralPath $data -Recurse -Force }

if ($script:failures.Count -gt 0) {
	throw "installer smoke test failed: $($script:failures -join '; ')"
}
Write-Host "installer smoke test passed"
