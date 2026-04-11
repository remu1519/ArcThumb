; ArcThumb installer script (Inno Setup 6.x)
;
; Build with:
;   cargo build --release
;   iscc installer\arcthumb.iss
;
; Output: target\installer\ArcThumb-Setup.exe
;
; The installer is per-user (no admin required) and installs to
;   %LOCALAPPDATA%\Programs\ArcThumb
;
; Post-install: silently calls `arcthumb-config.exe --install` to
; register the shell extension in HKCU. The Finish page offers a
; checkbox to launch the configuration GUI.
;
; Pre-uninstall: silently calls `arcthumb-config.exe --uninstall`
; to clean up the registry, then removes the files.

#define MyAppName       "ArcThumb"
#define MyAppVersion    "0.4.0"
#define MyAppPublisher  "citrussoda-com"
#define MyAppURL        "https://github.com/citrussoda-com/ArcThumb"
#define MyAppExeName    "arcthumb-config.exe"

[Setup]
; AppId — never change. Identifies upgrades vs new installs.
AppId={{DFE16BE0-6554-4F21-BB11-51601FD3FEC8}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE-MIT
OutputDir=..\target\installer
OutputBaseFilename=ArcThumb-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; HKCU only — no UAC prompt.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; 64-bit Explorer needs a 64-bit shell extension DLL.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Show the config exe as the icon in Apps & Features. The exe's
; embedded icon (resource ID 1, set up by `resources/arcthumb-config.rc`)
; is what Apps & Features actually displays.
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName} {#MyAppVersion}
; Icon for the installer .exe itself (`ArcThumb-Setup.exe`).
SetupIconFile=..\assets\icon.ico

[Languages]
Name: "english";  MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Files]
; Shell extension DLL — the actual thumbnail provider.
Source: "..\target\release\arcthumb.dll";        DestDir: "{app}"; Flags: ignoreversion
; Configuration GUI + CLI installer/uninstaller helper.
Source: "..\target\release\arcthumb-config.exe"; DestDir: "{app}"; Flags: ignoreversion
; Dual-license text files.
Source: "..\LICENSE-MIT";                        DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE";                     DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName} Configuration"; Filename: "{app}\{#MyAppExeName}"
Name: "{autoprograms}\Uninstall {#MyAppName}";     Filename: "{uninstallexe}"

[Run]
; Register the shell extension in HKCU. The DLL was just placed in
; {app} so `--install` finds it via `current_exe()`'s neighbour.
Filename: "{app}\{#MyAppExeName}"; Parameters: "--install"; \
    StatusMsg: "Registering shell extension..."; \
    Flags: runhidden waituntilterminated

; Finish-page checkbox. Launches the GUI if the user wants it.
Filename: "{app}\{#MyAppExeName}"; \
    Description: "Launch {#MyAppName} Configuration"; \
    Flags: postinstall nowait skipifsilent

[UninstallRun]
; Remove all shell-extension registry entries before files vanish.
; RunOnceId guards against the entry firing twice if the user
; cancels mid-uninstall and retries.
Filename: "{app}\{#MyAppExeName}"; Parameters: "--uninstall"; \
    RunOnceId: "ArcThumbUnregister"; \
    Flags: runhidden waituntilterminated

[UninstallDelete]
; Make sure the install dir disappears even if log files etc. exist.
Type: filesandordirs; Name: "{app}"
