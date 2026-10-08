#define AppVersion GetEnv("RACC_VERSION")
#if AppVersion == ""
  #error Set RACC_VERSION to the workspace version before compiling this installer.
#endif
#define StageDir "..\..\dist\racc-connect-" + AppVersion + "-windows-x64"

[Setup]
AppId={{68B93016-5484-5EA1-A135-EC4E91799F95}
AppName=Racc Connect
AppVersion={#AppVersion}
AppPublisher=Racc Connect (unsigned personal build)
DefaultDirName={autopf}\Racc Connect
DefaultGroupName=Racc Connect
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64
OutputDir=..\..\dist
OutputBaseFilename=racc-connect-{#AppVersion}-setup-windows-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\racc-app.exe

[Tasks]
Name: "appautostart"; Description: "Start Racc Connect when I sign in"; GroupDescription: "Startup options:"; Flags: unchecked
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Racc Connect"; Filename: "{app}\racc-app.exe"
Name: "{autodesktop}\Racc Connect"; Filename: "{app}\racc-app.exe"; Tasks: desktopicon

[Run]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\install-service.ps1"" --binary-path ""{app}\racc-host-agent.exe"""; StatusMsg: "Registering the Racc Connect host service..."; Flags: runhidden waituntilterminated
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\firewall-rules.ps1"" --agent-path ""{app}\racc-host-agent.exe"" --app-path ""{app}\racc-app.exe"""; StatusMsg: "Adding Tailscale-scoped firewall rules..."; Flags: runhidden waituntilterminated
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -Command ""Start-Service -Name 'RaccConnectHost'"""; StatusMsg: "Starting the host service..."; Flags: runhidden waituntilterminated
Filename: "{app}\racc-app.exe"; Description: "Launch Racc Connect"; Flags: postinstall nowait skipifsilent

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "RaccConnect"; ValueData: """{app}\racc-app.exe"""; Tasks: appautostart; Flags: uninsdeletevalue

[UninstallRun]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\firewall-rules-remove.ps1"""; Flags: runhidden waituntilterminated
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\uninstall-service.ps1"""; Flags: runhidden waituntilterminated
