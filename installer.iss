[Setup]
AppName=ClaudeBar Rust
AppVersion=0.4.68
AppPublisher=tddworks Clone
DefaultDirName={autopf}\ClaudeBarRust
DefaultGroupName=ClaudeBar Rust
UninstallDisplayIcon={app}\claudebar-rs.exe
Compression=lzma2
SolidCompression=yes
OutputDir=.\installer_output
OutputBaseFilename=ClaudeBar_Rust_Setup

[Files]
; Copia o executável compilado em modo Release pelo Cargo
Source: ".\target\release\claudebar-rs.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Atalho no Menu Iniciar
Name: "{group}\ClaudeBar Rust"; Filename: "{app}\claudebar-rs.exe"
; Atalho para iniciar automaticamente com o Windows (Startup)
Name: "{userstartup}\ClaudeBar Rust"; Filename: "{app}\claudebar-rs.exe"

[Run]
; Opção para iniciar a aplicação logo após terminar a instalação
Filename: "{app}\claudebar-rs.exe"; Description: "Lançar ClaudeBar Rust agora"; Flags: nowait postinstall skipifsilent