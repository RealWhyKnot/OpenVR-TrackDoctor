Unicode true

!define APPNAME "TrackDoctor"
!define EXENAME "trackdoctor.exe"
!define OLDEXE "trackdoctor-bg.exe"
!define UNINSTALLER "Uninstall.exe"
!define ARPKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\TrackDoctor"
!define DATADIR "$LOCALAPPDATA\trackdoctor"
!define OLDMENUDIR "$SMPROGRAMS\TrackDoctor"

Name "${APPNAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\OpenVR-TrackDoctor"
InstallDirRegKey HKCU "${ARPKEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "..\assets\icon.ico"
!define MUI_UNICON "..\assets\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXENAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Open TrackDoctor"
!define MUI_FINISHPAGE_TEXT "TrackDoctor opens its window when SteamVR starts and records while that window is open. Close the window and it stops.$\r$\n$\r$\nYou can turn off opening with SteamVR in TrackDoctor's Settings page."

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
		!insertmacro IsRunning "${OLDEXE}"
		${If} $0 = 0
			StrCpy $1 "An older TrackDoctor is recording because SteamVR is running. Close SteamVR, then try again."
			StrCpy $2 6
		${Else}
			!insertmacro IsRunning "${EXENAME}"
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
	ReadRegStr $0 HKCU "${ARPKEY}" "InstallLocation"
	${If} $0 != ""
	${AndIf} $0 != $INSTDIR
	${AndIf} ${FileExists} "$0\${UNINSTALLER}"
		DetailPrint "Removing the previous version from $0"
		ExecWait '"$0\${UNINSTALLER}" /S _?=$0' $1
		${If} $1 != 0
			DetailPrint "The previous uninstaller returned $1"
		${EndIf}
		Delete "$0\${UNINSTALLER}"
		RMDir "$0"
	${EndIf}

	Delete "$INSTDIR\${OLDEXE}"
	Delete "${OLDMENUDIR}\TrackDoctor.lnk"
	Delete "${OLDMENUDIR}\TrackDoctor last session report.lnk"
	Delete "${OLDMENUDIR}\TrackDoctor USB layout.lnk"
	Delete "${OLDMENUDIR}\Recorded sessions.lnk"
	Delete "${OLDMENUDIR}\Uninstall TrackDoctor.lnk"
	RMDir "${OLDMENUDIR}"

	SetOutPath "$INSTDIR"
	File "${PAYLOAD}\${EXENAME}"
	File "${PAYLOAD}\README.md"
	File "${PAYLOAD}\LICENSE"
	WriteUninstaller "$INSTDIR\${UNINSTALLER}"
	CreateShortcut "$SMPROGRAMS\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"

	WriteRegStr HKCU "${ARPKEY}" "DisplayName" "${APPNAME}"
	WriteRegStr HKCU "${ARPKEY}" "DisplayVersion" "${VERSION}"
	WriteRegStr HKCU "${ARPKEY}" "DisplayIcon" "$INSTDIR\${EXENAME}"
	WriteRegStr HKCU "${ARPKEY}" "Publisher" "RealWhyKnot"
	WriteRegStr HKCU "${ARPKEY}" "InstallLocation" "$INSTDIR"
	WriteRegStr HKCU "${ARPKEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
	WriteRegStr HKCU "${ARPKEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
	WriteRegStr HKCU "${ARPKEY}" "URLInfoAbout" "https://github.com/RealWhyKnot/OpenVR-TrackDoctor"
	WriteRegDWORD HKCU "${ARPKEY}" "NoModify" 1
	WriteRegDWORD HKCU "${ARPKEY}" "NoRepair" 1
	${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
	WriteRegDWORD HKCU "${ARPKEY}" "EstimatedSize" $0

	DetailPrint "Adding TrackDoctor to SteamVR's startup apps..."
	nsExec::ExecToLog /TIMEOUT=60000 '"$INSTDIR\${EXENAME}" --register-steamvr'
	Pop $0
	${If} $0 != "0"
	${AndIf} $0 != "3"
		DetailPrint "Adding TrackDoctor to SteamVR didn't finish ($0). Turn on $\"Open TrackDoctor when SteamVR starts$\" in its Settings page to try again."
	${EndIf}
SectionEnd

Section "Uninstall"
	DetailPrint "Removing TrackDoctor from SteamVR's startup apps..."
	nsExec::ExecToLog /TIMEOUT=60000 '"$INSTDIR\${EXENAME}" --unregister-steamvr'
	Pop $0
	${If} $0 != "0"
		DetailPrint "Removing TrackDoctor from SteamVR didn't finish ($0)."
	${EndIf}

	Delete "$SMPROGRAMS\${APPNAME}.lnk"
	Delete "${OLDMENUDIR}\TrackDoctor.lnk"
	Delete "${OLDMENUDIR}\TrackDoctor last session report.lnk"
	Delete "${OLDMENUDIR}\TrackDoctor USB layout.lnk"
	Delete "${OLDMENUDIR}\Recorded sessions.lnk"
	Delete "${OLDMENUDIR}\Uninstall TrackDoctor.lnk"
	RMDir "${OLDMENUDIR}"
	DeleteRegKey HKCU "${ARPKEY}"

	Delete "$INSTDIR\${EXENAME}"
	Delete "$INSTDIR\${OLDEXE}"
	Delete "$INSTDIR\manifest.vrmanifest"
	Delete "$INSTDIR\README.md"
	Delete "$INSTDIR\LICENSE"
	Delete "$INSTDIR\${UNINSTALLER}"
	RMDir "$INSTDIR"
	RMDir /r "${DATADIR}\update"

	${IfNot} ${Silent}
		MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Also delete your recorded sessions, device names and settings?$\r$\n$\r$\n${DATADIR}" IDNO keep_data
		RMDir /r "${DATADIR}"
		keep_data:
	${EndIf}
SectionEnd
