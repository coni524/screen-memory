; Inno Setup 6 installer definition.
; Build it through packaging\build-installer.ps1, which passes the version in via /DAppVersion.
;
; This is a per-user install with no administrator rights. Both the background
; agent (the Task Scheduler entry) and the settings and state (%APPDATA%,
; %LOCALAPPDATA%) are per-user, so there is no reason to elevate.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8B54A1E3-4C56-4D8B-9F0E-2D1A7C3E9B42}
AppName=Screen Memory
AppVersion={#AppVersion}
DefaultDirName={autopf}\screen-memory
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=screen-memory-setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
InfoAfterFile=installer-readme.txt
UninstallDisplayIcon={app}\screen-memory.exe
; PrepareToInstall stops the running process itself, so don't use Restart Manager
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[CustomMessages]
english.RegisteringTask=Registering the scheduled task...
japanese.RegisteringTask=タスクスケジューラに登録しています...
english.LaunchNow=Launch Screen Memory now
japanese.LaunchNow=Screen Memory を今すぐ起動する

[Files]
Source: "..\target\release\screen-memory.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "register-windows-task.ps1"; DestDir: "{app}"; Flags: ignoreversion

[Run]
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File ""{app}\register-windows-task.ps1"" ""{app}\screen-memory.exe"""; \
  Flags: runhidden; StatusMsg: "{cm:RegisteringTask}"
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -WindowStyle Hidden -Command Start-ScheduledTask -TaskName screen-memory"; \
  Flags: runhidden postinstall nowait; Description: "{cm:LaunchNow}"

[UninstallRun]
; Order matters: stop the running agent first, then remove the task registration
Filename: "taskkill.exe"; Parameters: "/F /IM screen-memory.exe"; \
  Flags: runhidden; RunOnceId: "KillAgent"
Filename: "powershell.exe"; \
  Parameters: "-NoProfile -WindowStyle Hidden -Command ""Unregister-ScheduledTask -TaskName screen-memory -Confirm:$false"""; \
  Flags: runhidden; RunOnceId: "UnregisterTask"

[Code]
// An install over an existing one fails if the exe is locked, so stop the agent
// first. If it isn't running, taskkill simply does nothing, which is harmless.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec('taskkill.exe', '/F /IM screen-memory.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;
