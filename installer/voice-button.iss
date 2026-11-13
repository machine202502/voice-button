#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif
#ifndef ThirdParty
  #define ThirdParty "..\dist\THIRD-PARTY.txt"
#endif
#ifndef RedistDir
  #define RedistDir "..\dist\redist"
#endif

[Setup]
AppId={{B7B65B4A-2D91-4D0B-9DF2-5E4FC8A0A6D1}
AppName=Voice Button
AppVersion={#AppVersion}
AppPublisher=machine202502
AppPublisherURL=https://github.com/machine202502/voice-button
AppSupportURL=https://github.com/machine202502/voice-button/issues
AppUpdatesURL=https://github.com/machine202502/voice-button/releases
DefaultDirName={autopf}\Voice Button
DefaultGroupName=Voice Button
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=voice-button-{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\voice-button.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
CloseApplications=yes
LicenseFile=..\LICENSE
InfoBeforeFile=..\NOTICE

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\voice-button.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\voice-button-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\sherpa-onnx-c-api.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\sherpa-onnx-cxx-api.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\onnxruntime.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RedistDir}\msvcp140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RedistDir}\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RedistDir}\vcruntime140_1.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#RedistDir}\vulkan-1.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#RedistDir}\vc_redist.x64.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\AISOFTWARE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\CREDITS.md"; DestName: "CREDITS.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\locales\*.json"; DestDir: "{app}\locales"; Flags: ignoreversion
; The phone page and the network script ship with the app: the phone opens the
; page from this folder, and the script opens the port for it.
Source: "..\src\web\*"; DestDir: "{app}\site-app"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\scripts\net.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ThirdParty}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Voice Button"; Filename: "{app}\voice-button.exe"; Parameters: "--guard"
Name: "{autodesktop}\Voice Button"; Filename: "{app}\voice-button.exe"; Parameters: "--guard"; Tasks: desktopicon

[Run]
Filename: "{tmp}\vc_redist.x64.exe"; Parameters: "/install /quiet /norestart"; StatusMsg: "Installing Microsoft Visual C++ Runtime..."; Flags: waituntilterminated; Check: NeedsVCRedist
Filename: "{app}\voice-button.exe"; Parameters: "--guard"; Description: "Launch Voice Button"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Nothing may be left open behind the app: the firewall rule and, if it was
; created, the router mapping are removed with it.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\net.ps1"" -Action disable"; Flags: waituntilterminated runhidden; RunOnceId: "RemovePhoneAccess"

[Code]
function NeedsVCRedist: Boolean;
var
  Installed: Cardinal;
begin
  Result := True;
  if RegQueryDWordValue(HKLM64, 'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64', 'Installed', Installed) then
    Result := Installed <> 1;
end;
