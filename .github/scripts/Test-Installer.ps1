#Requires -Version 5.1
[CmdletBinding()]
param(
	[Parameter(Mandatory = $true)][string]$Setup,
	[Parameter(Mandatory = $true)][string]$Version,
	[string]$Payload
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

function Invoke-Exe([string]$Path, [string]$Arguments, [int]$TimeoutSeconds = 180) {
	$process = Start-Process -FilePath $Path -ArgumentList $Arguments -PassThru
	$null = $process.Handle
	if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
		Stop-Process -Id $process.Id -Force
		return "timeout"
	}
	return $process.ExitCode
}

function Write-Text([string]$Path, [string]$Text) {
	New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
	[System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

function Add-OldInstallFiles([string]$Dir) {
	New-Item -ItemType File -Force -Path (Join-Path $Dir "trackdoctor-bg.exe") | Out-Null
	New-Item -ItemType Directory -Force -Path $oldMenu | Out-Null
	foreach ($l in $oldLinks) {
		New-Item -ItemType File -Force -Path (Join-Path $oldMenu $l) | Out-Null
	}
}

function Assert-Installed([string]$Dir) {
	foreach ($f in $installed) {
		$path = Join-Path $Dir $f
		$present = Test-Path -LiteralPath $path
		Assert $present "installed $f in $Dir"
		if ($present) {
			$same = (Get-FileHash -LiteralPath $path).Hash -eq (Get-FileHash -LiteralPath (Join-Path $payloadDir $f)).Hash
			Assert $same "$f matches the payload"
		}
	}
	Assert (Test-Path -LiteralPath (Join-Path $Dir "Uninstall.exe")) "uninstaller written to $Dir"
	Assert (-not (Test-Path -LiteralPath (Join-Path $Dir "trackdoctor-bg.exe"))) "no background recorder in $Dir"
	Assert (-not (Test-Path -LiteralPath $oldMenu)) "old start menu folder gone"

	$exe = Join-Path $Dir "trackdoctor.exe"
	$arp = Get-ItemProperty -Path $arpPath
	Assert ($arp.DisplayName -eq "TrackDoctor") "DisplayName (got $($arp.DisplayName))"
	Assert ($arp.DisplayVersion -eq $Version) "DisplayVersion $Version (got $($arp.DisplayVersion))"
	Assert ($arp.Publisher -eq "RealWhyKnot") "Publisher (got $($arp.Publisher))"
	Assert ($arp.InstallLocation -eq $Dir) "InstallLocation $Dir (got $($arp.InstallLocation))"
	Assert ($arp.DisplayIcon -eq $exe) "DisplayIcon (got $($arp.DisplayIcon))"
	Assert ($arp.UninstallString -eq ('"' + (Join-Path $Dir "Uninstall.exe") + '"')) "UninstallString (got $($arp.UninstallString))"
	Assert ($arp.QuietUninstallString -eq ('"' + (Join-Path $Dir "Uninstall.exe") + '" /S')) "QuietUninstallString (got $($arp.QuietUninstallString))"
	Assert ($arp.NoModify -eq 1 -and $arp.NoRepair -eq 1) "NoModify and NoRepair"
	Assert ($arp.EstimatedSize -gt 0) "EstimatedSize (got $($arp.EstimatedSize))"

	$target = ""
	$workingDir = ""
	if (Test-Path -LiteralPath $shortcut) {
		$link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
		$target = $link.TargetPath
		$workingDir = $link.WorkingDirectory
	}
	Assert ($target -eq $exe) "start menu shortcut targets $exe (got '$target')"
	Assert ($workingDir -eq $Dir) "start menu shortcut starts in $Dir (got '$workingDir')"
}

function Invoke-Uninstall([string]$Dir) {
	$code = Invoke-Exe (Join-Path $Dir "Uninstall.exe") "/S _?=$Dir"
	Assert ("$code" -eq "0") "uninstaller exit code 0 (got $code)"
	Assert (-not (Test-Path -Path $arpPath)) "uninstall entry removed"
	Assert (-not (Test-Path -LiteralPath $shortcut)) "start menu shortcut removed"
	Assert (-not (Test-Path -LiteralPath $oldMenu)) "old start menu folder removed"
	$left = @()
	if (Test-Path -LiteralPath $Dir) {
		$left = @(Get-ChildItem -LiteralPath $Dir -Recurse -Force | Where-Object { $_.Name -ne "Uninstall.exe" })
	}
	Assert ($left.Count -eq 0) "install folder emptied (left: $(($left | ForEach-Object Name) -join ', '))"
	Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue
}

$setupPath = (Resolve-Path -LiteralPath $Setup).Path
if (-not $Payload) { $Payload = Join-Path (Split-Path -Parent $setupPath) "OpenVR-TrackDoctor-$Version" }
$payloadDir = (Resolve-Path -LiteralPath $Payload).Path.TrimEnd('\', '/')
$installed = @("trackdoctor.exe", "README.md", "LICENSE")
$arpPath = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\TrackDoctor"
$oldLinks = @("TrackDoctor.lnk", "TrackDoctor last session report.lnk", "TrackDoctor USB layout.lnk", "Recorded sessions.lnk", "Uninstall TrackDoctor.lnk")
$programs = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$shortcut = Join-Path $programs "TrackDoctor.lnk"
$oldMenu = Join-Path $programs "TrackDoctor"
Add-Type -Namespace InstallerTest -Name Native -MemberDefinition '[DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern uint GetLongPathName(string shortPath, System.Text.StringBuilder longPath, uint size);'
$longTemp = New-Object System.Text.StringBuilder 1024
if ([InstallerTest.Native]::GetLongPathName($env:TEMP, $longTemp, 1024) -eq 0) { throw "GetLongPathName failed for $env:TEMP" }
$root = Join-Path $longTemp.ToString() "TrackDoctor Setup Smoke"
$first = Join-Path $root "first"
$second = Join-Path $root "second"

$vrPaths = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "openvr\openvrpaths.vrpath"
if (Test-Path -LiteralPath $vrPaths) { throw "refusing to run: SteamVR is installed on this machine and setup would change its startup apps" }
if (Test-Path -Path $arpPath) { throw "refusing to run: TrackDoctor is installed on this machine and the test would remove it" }
if (Test-Path -LiteralPath $shortcut) { throw "refusing to run: $shortcut exists and the test would remove it" }
if (Test-Path -LiteralPath $oldMenu) { throw "refusing to run: $oldMenu exists and the test would remove it" }
if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }

$realLocal = $env:LOCALAPPDATA
$shellData = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "trackdoctor"
$shellUpdate = Join-Path $shellData "update"
$shellDataExisted = Test-Path -LiteralPath $shellData
if (Test-Path -LiteralPath $shellUpdate) { throw "refusing to run: $shellUpdate exists and the uninstaller would remove it" }
$fakeLocal = Join-Path $root "profile\Local"
$data = Join-Path $fakeLocal "trackdoctor"
$settings = Join-Path $data "settings.json"

try {
	New-Item -ItemType Directory -Force -Path $fakeLocal | Out-Null
	$env:LOCALAPPDATA = $fakeLocal

	$code = Invoke-Exe $setupPath "/S /D=$first"
	Assert ("$code" -eq "0") "fresh install exit code 0 (got $code)"
	Assert-Installed $first
	Assert (Test-Path -LiteralPath (Join-Path $first "manifest.vrmanifest")) "setup wrote the SteamVR manifest beside the exe"
	$manifest = Get-Content -LiteralPath (Join-Path $first "manifest.vrmanifest") -Raw | ConvertFrom-Json
	$app = $manifest.applications[0]
	Assert ($app.binary_path_windows -eq "trackdoctor.exe" -and $app.arguments -eq "--steamvr") "SteamVR opens trackdoctor.exe --steamvr (got $($app.binary_path_windows) $($app.arguments))"

	Add-OldInstallFiles $first
	$code = Invoke-Exe $setupPath "/S /D=$second"
	Assert ("$code" -eq "0") "install into a new folder exit code 0 (got $code)"
	Assert (-not (Test-Path -LiteralPath $first)) "moving the install removed the old folder"
	Assert-Installed $second

	Add-OldInstallFiles $second
	Write-Text (Join-Path $second "manifest.vrmanifest") '{"applications": [{"binary_path_windows": "trackdoctor-bg.exe"}]}'
	$code = Invoke-Exe $setupPath "/S /D=$second"
	Assert ("$code" -eq "0") "upgrade over an older install exit code 0 (got $code)"
	Assert-Installed $second
	$manifest = Get-Content -LiteralPath (Join-Path $second "manifest.vrmanifest") -Raw | ConvertFrom-Json
	Assert ($manifest.applications[0].binary_path_windows -eq "trackdoctor.exe") "upgrade pointed the old SteamVR manifest at trackdoctor.exe"
	$noted = (Test-Path -LiteralPath $settings) -and ((Get-Content -LiteralPath $settings -Raw | ConvertFrom-Json).old_manifest -eq $true)
	Assert $noted "upgrade remembered the old manifest so an old autostart-off choice can be kept"

	$code = Invoke-Exe (Join-Path $second "trackdoctor.exe") "--unregister-steamvr" 60
	Assert ("$code" -eq "3") "--unregister-steamvr exits 3 when SteamVR can't be loaded instead of opening the window (got $code)"

	Remove-Item -LiteralPath (Join-Path $second "manifest.vrmanifest") -Force
	Write-Text $settings '{"open_with_steamvr": false, "save_poses": true}'
	$code = Invoke-Exe $setupPath "/S /D=$second"
	Assert ("$code" -eq "0") "reinstall with opening from SteamVR turned off exit code 0 (got $code)"
	Assert (-not (Test-Path -LiteralPath (Join-Path $second "manifest.vrmanifest"))) "setup left SteamVR alone when opening from SteamVR is off"
	$kept = Get-Content -LiteralPath $settings -Raw | ConvertFrom-Json
	Assert ($kept.open_with_steamvr -eq $false -and $kept.save_poses -eq $true) "setup kept the settings"

	$session = Join-Path $data "sessions\session-1\events.jsonl"
	$staged = Join-Path $shellUpdate "setup-smoke.bin"
	foreach ($seed in @($session, $staged)) {
		New-Item -ItemType File -Force -Path $seed | Out-Null
	}
	Add-OldInstallFiles $second
	Invoke-Uninstall $second
	Assert (Test-Path -LiteralPath $session) "silent uninstall kept recorded sessions"
	Assert (Test-Path -LiteralPath $settings) "silent uninstall kept settings"
	Assert (-not (Test-Path -LiteralPath (Split-Path -Parent $staged))) "uninstall removed the update staging folder"
}
finally {
	$env:LOCALAPPDATA = $realLocal
	if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
	if (Test-Path -Path $arpPath) { Remove-Item -Path $arpPath -Recurse -Force }
	if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut -Force }
	if (Test-Path -LiteralPath $oldMenu) { Remove-Item -LiteralPath $oldMenu -Recurse -Force }
	if (Test-Path -LiteralPath $shellUpdate) { Remove-Item -LiteralPath $shellUpdate -Recurse -Force }
	if (-not $shellDataExisted -and (Test-Path -LiteralPath $shellData) -and -not (Get-ChildItem -LiteralPath $shellData -Force)) { Remove-Item -LiteralPath $shellData -Force }
}

if ($script:failures.Count -gt 0) {
	throw "installer smoke test failed: $($script:failures -join '; ')"
}
Write-Host "installer smoke test passed"
