Unicode true

!define APPNAME "TrackDoctor"
!define ARPKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\TrackDoctor"
!define DATADIR "$LOCALAPPDATA\trackdoctor"
!define MENUDIR "$SMPROGRAMS\TrackDoctor"

Name "${APPNAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\TrackDoctor"
InstallDirRegKey HKCU "${ARPKEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_FINISHPAGE_RUN "$INSTDIR\trackdoctor.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open the TrackDoctor live view now"
!define MUI_FINISHPAGE_TEXT "TrackDoctor now starts by itself whenever SteamVR runs and records tracking problems in the background.$\r$\n$\r$\nAfter a VR session, open $\"TrackDoctor last session report$\" from the Start menu to see which trackers did worst."

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${VIVERSION}"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APPNAME} Setup"
VIAddVersionKey "LegalCopyright" ""

!macro IsRunning EXE
	nsExec::ExecToStack 'cmd /c tasklist /NH /FI "IMAGENAME eq ${EXE}" | find /I "${EXE}"'
	Pop $0
	Pop $3
!macroend

!macro GuardRunning
	${Do}
		StrCpy $1 ""
		!insertmacro IsRunning "trackdoctor-bg.exe"
		${If} $0 = 0
			StrCpy $1 "TrackDoctor is recording because SteamVR is running. Close SteamVR, then try again."
			StrCpy $2 6
		${Else}
			!insertmacro IsRunning "trackdoctor.exe"
			${If} $0 = 0
				StrCpy $1 "TrackDoctor is open. Close it, then try again."
				StrCpy $2 5
			${EndIf}
		${EndIf}
		${If} $1 == ""
			${Break}
		${EndIf}
		${If} ${Silent}
			SetErrorLevel $2
			Quit
		${EndIf}
		MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$1" IDRETRY +2
		Quit
	${Loop}
!macroend

Function .onInit
	!insertmacro GuardRunning
FunctionEnd

Function un.onInit
	!insertmacro GuardRunning
FunctionEnd

Section "Install"
	SetOutPath "$INSTDIR"
	File "${PAYLOAD}\trackdoctor.exe"
	File "${PAYLOAD}\trackdoctor-bg.exe"
	File "${PAYLOAD}\README.md"
	File "${PAYLOAD}\LICENSE"
	WriteUninstaller "$INSTDIR\Uninstall.exe"

	CreateDirectory "${DATADIR}\sessions"
	CreateDirectory "${MENUDIR}"
	CreateShortcut "${MENUDIR}\TrackDoctor.lnk" "$INSTDIR\trackdoctor.exe"
	CreateShortcut "${MENUDIR}\TrackDoctor last session report.lnk" "$INSTDIR\trackdoctor.exe" "report"
	CreateShortcut "${MENUDIR}\TrackDoctor USB layout.lnk" "$INSTDIR\trackdoctor.exe" "usb"
	CreateShortcut "${MENUDIR}\Recorded sessions.lnk" "${DATADIR}\sessions"
	CreateShortcut "${MENUDIR}\Uninstall TrackDoctor.lnk" "$INSTDIR\Uninstall.exe"

	WriteRegStr HKCU "${ARPKEY}" "DisplayName" "${APPNAME}"
	WriteRegStr HKCU "${ARPKEY}" "DisplayVersion" "${VERSION}"
	WriteRegStr HKCU "${ARPKEY}" "DisplayIcon" "$INSTDIR\trackdoctor.exe"
	WriteRegStr HKCU "${ARPKEY}" "Publisher" "RealWhyKnot"
	WriteRegStr HKCU "${ARPKEY}" "InstallLocation" "$INSTDIR"
	WriteRegStr HKCU "${ARPKEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
	WriteRegStr HKCU "${ARPKEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
	WriteRegStr HKCU "${ARPKEY}" "URLInfoAbout" "https://github.com/RealWhyKnot/OpenVR-TrackDoctor"
	WriteRegDWORD HKCU "${ARPKEY}" "NoModify" 1
	WriteRegDWORD HKCU "${ARPKEY}" "NoRepair" 1
	${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
	WriteRegDWORD HKCU "${ARPKEY}" "EstimatedSize" $0

	DetailPrint "Registering with SteamVR so TrackDoctor starts with VR..."
	nsExec::ExecToLog '"$INSTDIR\trackdoctor.exe" autostart on'
	Pop $0
	${If} $0 <> 0
		DetailPrint "SteamVR registration did not complete (code $0)."
		${IfNot} ${Silent}
			MessageBox MB_OK|MB_ICONINFORMATION "TrackDoctor is installed, but it could not register with SteamVR, so it will not start with VR yet. Install or update SteamVR, then run this installer again."
		${EndIf}
	${EndIf}
SectionEnd

Section "Uninstall"
	DetailPrint "Removing TrackDoctor from SteamVR's startup apps..."
	nsExec::ExecToLog '"$INSTDIR\trackdoctor.exe" autostart off'
	Pop $0

	Delete "${MENUDIR}\TrackDoctor.lnk"
	Delete "${MENUDIR}\TrackDoctor last session report.lnk"
	Delete "${MENUDIR}\TrackDoctor USB layout.lnk"
	Delete "${MENUDIR}\Recorded sessions.lnk"
	Delete "${MENUDIR}\Uninstall TrackDoctor.lnk"
	RMDir "${MENUDIR}"
	DeleteRegKey HKCU "${ARPKEY}"

	Delete "$INSTDIR\trackdoctor.exe"
	Delete "$INSTDIR\trackdoctor-bg.exe"
	Delete "$INSTDIR\manifest.vrmanifest"
	Delete "$INSTDIR\README.md"
	Delete "$INSTDIR\LICENSE"
	Delete "$INSTDIR\Uninstall.exe"
	RMDir "$INSTDIR"

	${IfNot} ${Silent}
		MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Also delete your recorded sessions and device names?$\r$\n$\r$\n${DATADIR}" IDNO keep_data
		RMDir /r "${DATADIR}"
		keep_data:
	${EndIf}
SectionEnd
