#define MyAppName      "ClaudTray"
#define MyAppVersion   "1.1.1"
#define MyAppPublisher "Victor Magne"
#define MyAppExeName   "claudtray.exe"
#define MyAppURL       "https://github.com/Victor-Magne/claudtray"

[Setup]
AppId={{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
AppComments=Monitor de uso de assistentes de IA (Claude, Codex, Antigravity, Copilot)

; Machine-wide install in 64-bit Program Files — requires admin elevation
DefaultDirName={commonpf64}\{#MyAppName}
ArchitecturesInstallIn64BitMode=x64compatible
DefaultGroupName={#MyAppName}
PrivilegesRequired=admin
UsedUserAreasWarning=no

DisableProgramGroupPage=yes
DisableDirPage=no
WizardStyle=modern

; Output
OutputDir=.\installer_output
OutputBaseFilename=ClaudTray
Compression=lzma2/ultra64
SolidCompression=yes

; Branding
SetupIconFile=assets\claudtray.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

[Messages]
WelcomeLabel2=Bem-vindo ao instalador do [name]!%n%nEste programa monitoriza o uso de assistentes de IA (Claude Code, Codex, Antigravity, GitHub Copilot) em tempo real na barra de tarefas do Windows.%n%nClica em Seguinte para continuar.
FinishedLabel=A instalação do [name] está concluída.%n%nPodes iniciá-lo a qualquer momento pelo Menu Iniciar ou procurando por "ClaudTray".

[Files]
; App executable (CRT statically linked — no MSVC Redist needed)
Source: "target\x86_64-pc-windows-msvc\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

; App icon
Source: "assets\claudtray.ico"; DestDir: "{app}"; Flags: ignoreversion

; WebView2 Evergreen Bootstrapper — extracted to temp only when needed
; (~1.6 MB, downloads from Microsoft if WebView2 is absent on the machine)
Source: "assets\MicrosoftEdgeWebview2Setup.exe"; DestDir: "{tmp}"; \
  Flags: dontcopy deleteafterinstall

[Icons]
; Start Menu — searchable via Windows Search
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  IconFilename: "{app}\claudtray.ico"; \
  Comment: "Monitor de uso de IA em tempo real"
Name: "{group}\Desinstalar {#MyAppName}"; Filename: "{uninstallexe}"

[Tasks]
Name: "startup"; Description: "Iniciar {#MyAppName} automaticamente com o Windows"; \
  GroupDescription: "Opções adicionais:"; Flags: unchecked

[Code]

function GetInstalledVersion: String;
begin
  Result := '';
  RegQueryStringValue(HKLM,
    'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}_is1',
    'DisplayVersion', Result);
end;

// Extrai a N-ésima parte (0-based) de uma versão "X.Y.Z.W"
function GetVersionPart(const Ver: String; Part: Integer): Integer;
var
  S: String;
  I, Count: Integer;
begin
  S := Ver;
  Count := 0;
  Result := 0;
  repeat
    I := Pos('.', S);
    if I > 0 then begin
      if Count = Part then begin
        Result := StrToIntDef(Copy(S, 1, I - 1), 0);
        Exit;
      end;
      Delete(S, 1, I);
      Count := Count + 1;
    end else begin
      if Count = Part then
        Result := StrToIntDef(S, 0);
      Break;
    end;
  until False;
end;

// Retorna -1 se A < B, 0 se A = B, 1 se A > B
function CompareVersionStrings(const A, B: String): Integer;
var
  I, PA, PB: Integer;
begin
  Result := 0;
  for I := 0 to 3 do begin
    PA := GetVersionPart(A, I);
    PB := GetVersionPart(B, I);
    if PA < PB then begin Result := -1; Exit; end
    else if PA > PB then begin Result := 1; Exit; end;
  end;
end;

function InitializeSetup: Boolean;
var
  InstalledVer: String;
  Cmp: Integer;
begin
  Result := True;
  InstalledVer := GetInstalledVersion;
  if InstalledVer = '' then Exit;

  Cmp := CompareVersionStrings('{#MyAppVersion}', InstalledVer);

  if Cmp = 0 then begin
    Result := MsgBox(
      'O ClaudTray ' + InstalledVer + ' já está instalado.' + #13#10 + #13#10 +
      'Deseja reinstalar a mesma versão?',
      mbConfirmation, MB_YESNO) = IDYES;
  end else if Cmp > 0 then begin
    Result := MsgBox(
      'O ClaudTray ' + InstalledVer + ' está instalado.' + #13#10 + #13#10 +
      'Deseja atualizar para a versão {#MyAppVersion}?',
      mbConfirmation, MB_YESNO) = IDYES;
  end else begin
    // Downgrade: versão instalada é mais recente
    Result := MsgBox(
      'O ClaudTray ' + InstalledVer + ' está instalado e é mais recente do que esta versão ({#MyAppVersion}).' + #13#10 + #13#10 +
      'Deseja mesmo assim instalar a versão mais antiga?',
      mbConfirmation, MB_YESNO) = IDYES;
  end;
end;

// Migrate old per-user / x86 install: silently uninstall it before setup begins.
procedure MigrateOldInstall;
var
  UninstStr: String;
  ResultCode: Integer;
begin
  if RegQueryStringValue(HKLM,
      'SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}_is1',
      'UninstallString', UninstStr) then begin
    Exec(RemoveQuotes(UninstStr), '/SILENT', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
  end;
  if RegQueryStringValue(HKCU,
      'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{F3A7B2C1-8D4E-4F9A-B6C2-1E5D7A3F8B9C}_is1',
      'UninstallString', UninstStr) then begin
    Exec(RemoveQuotes(UninstStr), '/SILENT', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
  end;
end;

// Remove scheduled task and kill the process on uninstall
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then begin
    Exec('taskkill', '/f /im {#MyAppExeName}', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
    Exec(ExpandConstant('{sys}\schtasks.exe'),
         '/Delete /TN "{#MyAppName}" /F', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
  end;
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
var
  ResultCode: Integer;
  ExePath: String;
begin
  if CurStep = ssInstall then begin
    MigrateOldInstall;
    if not WebView2Present then
      InstallWebView2;
  end;
  if CurStep = ssPostInstall then begin
    ExePath := ExpandConstant('{app}\{#MyAppExeName}');
    // Always remove any previous task first (upgrade / reinstall).
    Exec(ExpandConstant('{sys}\schtasks.exe'),
         '/Delete /TN "{#MyAppName}" /F', '', SW_HIDE,
         ewWaitUntilTerminated, ResultCode);
    if WizardIsTaskSelected('startup') then begin
      // Create an ONLOGON task for the current user — no UAC, no registry Run key.
      Exec(ExpandConstant('{sys}\schtasks.exe'),
           '/Create /TN "{#MyAppName}" /TR "\"' + ExePath + '\"" ' +
           '/SC ONLOGON /DELAY 0000:30 /F',
           '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    end;
  end;
end;

[Run]
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Lançar {#MyAppName} agora"; \
  Flags: nowait postinstall skipifsilent

