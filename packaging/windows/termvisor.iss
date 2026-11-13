; Inno Setup script for the Windows installer. Packs the folder made by
; bundle.sh. See docs/packaging.md.
;
;   iscc /DAppVersion=1.0.1 /DSource=dist\termvisor /Odist packaging\windows\termvisor.iss

#ifndef AppVersion
  #error Pass the version: /DAppVersion=x.y.z
#endif
#ifndef Source
  #define Source "..\..\dist\termvisor"
#endif

; Never change: Windows finds the installed copy by this id.
#define AppGuid "56F56441-95F8-436B-B4B5-D18E4081B9D5"

[Setup]
AppId={{{#AppGuid}}
AppName=termvisor
AppVersion={#AppVersion}
AppVerName=termvisor {#AppVersion}
AppPublisher=Yunus Kurt
AppPublisherURL=https://github.com/ykurt52/termvisor
AppSupportURL=https://github.com/ykurt52/termvisor/issues
AppUpdatesURL=https://github.com/ykurt52/termvisor/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\termvisor
DisableProgramGroupPage=yes
; Per user without admin rights; the dialog offers all users.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; ConPTY needs Windows 10 1809.
MinVersion=10.0.17763
LicenseFile=..\..\LICENSE
SetupIconFile=termvisor.ico
UninstallDisplayIcon={app}\termvisor.ico
UninstallDisplayName=termvisor
OutputBaseFilename=termvisor-windows-x64_v{#AppVersion}_setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; Files of the old version that the new one may not have.
Type: filesandordirs; Name: "{app}\lib"
Type: filesandordirs; Name: "{app}\share"
Type: files; Name: "{app}\*.dll"

[Files]
Source: "{#Source}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "termvisor.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\termvisor"; Filename: "{app}\termvisor.exe"; IconFilename: "{app}\termvisor.ico"
Name: "{autodesktop}\termvisor"; Filename: "{app}\termvisor.exe"; IconFilename: "{app}\termvisor.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\termvisor.exe"; Description: "{cm:LaunchProgram,termvisor}"; Flags: nowait postinstall skipifsilent
; Update from inside termvisor (/relaunch=1): start it again, not as admin.
Filename: "{app}\termvisor.exe"; Flags: nowait runasoriginaluser; Check: Relaunch

[Code]
function Relaunch(): Boolean;
begin
  Result := ExpandConstant('{param:relaunch|0}') = '1';
end;

const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{{#AppGuid}}_is1';

{ Version of an installed copy, per user or for all users; '' if none. }
function InstalledVersion(): String;
begin
  Result := '';
  if not RegQueryStringValue(HKCU, UninstallKey, 'DisplayVersion', Result) then
    if not RegQueryStringValue(HKLM64, UninstallKey, 'DisplayVersion', Result) then
      RegQueryStringValue(HKLM32, UninstallKey, 'DisplayVersion', Result);
end;

{ Same or older installed: install over it. Newer installed: stop. }
function InitializeSetup(): Boolean;
var
  Installed: String;
  Have, Want: Int64;
begin
  Result := True;
  Installed := InstalledVersion();
  if (Installed = '') or not StrToVersion(Installed, Have) or not StrToVersion('{#AppVersion}', Want) then
    Exit;
  if ComparePackedVersion(Have, Want) > 0 then
  begin
    SuppressibleMsgBox('termvisor ' + Installed + ' is installed, which is newer than ' +
      '{#AppVersion}. Uninstall it first to go back to this version.', mbError, MB_OK, IDOK);
    Result := False;
  end;
end;
