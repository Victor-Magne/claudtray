#define MyAppName      "CloudTray"
#define MyAppVersion   "1.0.0"
#define MyAppPublisher "Victor Magne"
#define MyAppExeName   "cloudtray.exe"
#define MyAppURL       "https://github.com/Victor-Magne/cloudtray"

[Setup]
AppId={{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppComments=Monitor de uso de assistentes de IA (Claude, Codex, Antigravity, Copilot)

; Machine-wide install in Program Files — requires admin elevation
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
PrivilegesRequired=admin

DisableProgramGroupPage=yes
DisableDirPage=no
WizardStyle=modern

; Output
OutputDir=.\installer_output
OutputBaseFilename=CloudTray_Setup_{#MyAppVersion}
Compression=lzma2/ultra64
SolidCompression=yes

; Branding
SetupIconFile=assets\cloudtray.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

[Messages]
WelcomeLabel2=Bem-vindo ao instalador do [name]!%n%nEste programa monitoriza o uso de assistentes de IA (Claude Code, Codex, Antigravity, GitHub Copilot) em tempo real na barra de tarefas do Windows.%n%nClica em Seguinte para continuar.
FinishedLabel=A instalação do [name] está concluída.%n%nPodes iniciá-lo a qualquer momento pelo Menu Iniciar ou procurando por "CloudTray".

[Files]
; App executable (CRT statically linked — no MSVC Redist needed)
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

; App icon
Source: "assets\cloudtray.ico"; DestDir: "{app}"; Flags: ignoreversion

; WebView2 Evergreen Bootstrapper — extracted to temp only when needed
; (~1.6 MB, downloads from Microsoft if WebView2 is absent on the machine)
Source: "assets\MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; \
  Flags: dontcopy deleteafterinstall

[Icons]
; Start Menu — searchable via Windows Search
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  IconFilename: "{app}\cloudtray.ico"; \
  Comment: "Monitor de uso de IA em tempo real"
Name: "{group}\Desinstalar {#MyAppName}"; Filename: "{uninstallexe}"

[Registry]
; Startup-with-Windows — only when the user ticks the checkbox
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "{#MyAppName}"; \
  ValueData: """{app}\{#MyAppExeName}"""; \
  Check: WantsStartup

[Tasks]
Name: "startup"; Description: "Iniciar {#MyAppName} automaticamente com o Windows"; \
  GroupDescription: "Opções adicionais:"; Flags: unchecked

[Code]
function WantsStartup: Boolean;
begin
  Result := WizardIsTaskSelected('startup');
end;

// Remove Run key on uninstall
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKCU,
      'Software\Microsoft\Windows\CurrentVersion\Run',
      '{#MyAppName}');
end;

// Check WebView2; if absent, silently install the bundled bootstrapper.
// The bootstrapper is tiny (~1.6 MB) and downloads the runtime from
// Microsoft. On Windows 11 WebView2 is always present so this never runs.
function WebView2Present: Boolean;
var
  Ver: String;
begin
  Result :=
    RegQueryStringValue(HKCU,
      'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv', Ver) or
    RegQueryStringValue(HKLM,
      'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv', Ver) or
    RegQueryStringValue(HKLM,
      'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv', Ver);
end;

procedure InstallWebView2;
var
  SetupPath: String;
  ResultCode: Integer;
begin
  ExtractTemporaryFile('MicrosoftEdgeWebview2Setup.exe');
  SetupPath := ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe');
  if not Exec(SetupPath, '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    MsgBox(
      'Não foi possível instalar o WebView2 Runtime automaticamente.' + #13#10 +
      'Instala manualmente em: https://aka.ms/webview2',
      mbError, MB_OK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
    if not WebView2Present then
      InstallWebView2;
end;

[Run]
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Lançar {#MyAppName} agora"; \
  Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "taskkill"; Parameters: "/f /im {#MyAppExeName}"; \
  Flags: runhidden; RunOnceId: "KillOnUninstall"
