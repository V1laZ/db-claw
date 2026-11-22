; DB Claw installer (Inno Setup 6).
;
; Per-user install into %LOCALAPPDATA%\Programs\DB Claw: no admin rights, no
; UAC prompt, and the app can update itself in place.
;
; iscc /DAppVersion=0.2.0 /DNumericVersion=0.2.0 /DBinDir=<dir with dbclaw.exe>
;      /DOutDir=<dir> /DOutName=<file name without .exe> packaging\windows\dbclaw.iss
;
; In-app updates run the new installer with
;   /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /UPDATE
; and /UPDATE starts DB Claw again when it's done.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif
#ifndef OutDir
  #define OutDir "..\..\dist"
#endif
#ifndef OutName
  #define OutName "db-claw-setup"
#endif

[Setup]
; Never change AppId: it ties updates and the uninstaller to earlier installs.
AppId={{2AC3BEAC-B5ED-4A89-BCED-524D8C820B0A}
AppName=DB Claw
AppVersion={#AppVersion}
AppVerName=DB Claw {#AppVersion}
AppPublisher=DB Claw contributors
AppPublisherURL=https://github.com/V1laZ/db-claw
AppSupportURL=https://github.com/V1laZ/db-claw/issues
AppUpdatesURL=https://github.com/V1laZ/db-claw/releases
VersionInfoVersion={#NumericVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\DB Claw
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
UninstallDisplayName=DB Claw
UninstallDisplayIcon={app}\dbclaw.exe
SetupIconFile=..\..\assets\icon.ico
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
RestartApplications=no
ChangesEnvironment=yes
OutputDir={#OutDir}
OutputBaseFilename={#OutName}

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add dbclaw-ctl (automation CLI / MCP server) to PATH"; GroupDescription: "Automation:"

[Files]
Source: "{#BinDir}\dbclaw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\dbclaw-ctl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\DB Claw"; Filename: "{app}\dbclaw.exe"
Name: "{autodesktop}\DB Claw"; Filename: "{app}\dbclaw.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; \
  Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
; Interactive install: "Launch DB Claw" checkbox on the last page.
Filename: "{app}\dbclaw.exe"; Description: "{cm:LaunchProgram,DB Claw}"; Flags: nowait postinstall skipifsilent
; In-app update: start the new version.
Filename: "{app}\dbclaw.exe"; Flags: nowait; Check: IsUpdate

[Code]
function IsUpdate: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/UPDATE') = 0 then
      Result := True;
end;

function PathContains(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    Paths := '';
  Result := not PathContains(Paths, Dir);
end;

procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    exit;
  Paths := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
  if P = 0 then
    exit;
  Delete(Paths, P, Length(Dir) + 1);
  Paths := Copy(Paths, 2, Length(Paths) - 2);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
