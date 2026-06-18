#define MyAppName      "ClaudeBar"
#define MyAppVersion   "1.0.0"
#define MyAppPublisher "Victor Gomes"
#define MyAppExeName   "claudebar-rs.exe"
#define MyAppURL       "https://github.com/tddworks/ClaudeBar"

[Setup]
AppId={{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppComments=Monitor de uso de assistentes de IA (Claude, Codex, Antigravity, Copilot)

; Install per-user — no admin required
DefaultDirName={localappdata}\{#MyAppName}
DefaultGroupName={#MyAppName}
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline

DisableProgramGroupPage=yes
DisableDirPage=no

; Modern Inno Setup wizard
WizardStyle=modern

; Output
OutputDir=.\installer_output
OutputBaseFilename=ClaudeBar_Setup_{#MyAppVersion}
Compression=lzma2/ultra64
SolidCompression=yes

; Branding
SetupIconFile=assets\claudebar.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

; WebView2 is required (comes with Windows 11, usually already present)
; If missing, guide the user to install it.
[Messages]
WelcomeLabel2=Bem-vindo ao instalador do [name]!%n%nEste programa monitoriza o uso de assistentes de IA (Claude Code, Codex, Antigravity, GitHub Copilot) em tempo real na barra de tarefas do Windows.%n%nClica em Seguinte para continuar.
FinishedLabel=A instalação do [name] está concluída.%n%nPodes iniciá-lo a qualquer momento pelo Menu Iniciar ou procurando por "ClaudeBar".

[Files]
; Main executable (with embedded icon and version info)
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

; App icon (used by shortcuts)
Source: "assets\claudebar.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Start Menu shortcut (searchable by Windows Search)
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\claudebar.ico"; Comment: "Monitor de uso de IA em tempo real"

; Uninstall shortcut in Start Menu
Name: "{group}\Desinstalar {#MyAppName}"; Filename: "{uninstallexe}"

[Registry]
; "Iniciar com o Windows" — written only when the user ticks the checkbox
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "{#MyAppName}"; \
  ValueData: """{app}\{#MyAppExeName}"""; \
  Check: WantsStartup

[Tasks]
; Optional startup-with-Windows task shown on the last wizard page
Name: "startup"; Description: "Iniciar {#MyAppName} automaticamente com o Windows"; \
  GroupDescription: "Opções adicionais:"; Flags: unchecked

[Code]
// Helper: returns True if the startup task is selected
function WantsStartup: Boolean;
begin
  Result := WizardIsTaskSelected('startup');
end;

// If user un-ticks startup after a previous install, remove the Run key
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKCU,
      'Software\Microsoft\Windows\CurrentVersion\Run',
      '{#MyAppName}');
end;

// Check for WebView2 runtime (required by the app). Warn but don't block.
function InitializeSetup(): Boolean;
var
  Ver: String;
begin
  Result := True;
  if not RegQueryStringValue(HKCU,
      'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv', Ver) then
  begin
    if not RegQueryStringValue(HKLM,
        'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'pv', Ver) then
    begin
      if MsgBox(
        'O WebView2 Runtime não foi detetado.' + #13#10 +
        'A ClaudeBar precisa dele para mostrar o painel.' + #13#10#13#10 +
        'No Windows 11 já vem incluído. Se a app não abrir o painel após a instalação,' + #13#10 +
        'instala o WebView2 em: https://aka.ms/webview2' + #13#10#13#10 +
        'Queres continuar mesmo assim?',
        mbConfirmation, MB_YESNO) = IDNO then
        Result := False;
    end;
  end;
end;

[Run]
; Offer to launch the app after install (runs in background, no window)
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Lançar {#MyAppName} agora"; \
  Flags: nowait postinstall skipifsilent

[UninstallRun]
; Make sure no instance is running when uninstalling
Filename: "taskkill"; Parameters: "/f /im {#MyAppExeName}"; \
  Flags: runhidden; RunOnceId: "KillOnUninstall"
