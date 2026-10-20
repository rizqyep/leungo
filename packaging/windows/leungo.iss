; Builds Leungo-<version>-windows-x64-setup.exe:
;   ISCC /DAppVersion=0.2.2 packaging\windows\leungo.iss
; Installs for the current user only, so Leungo itself needs no
; administrator rights. Equalizer APO does; its own installer asks.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef BinDir
  #define BinDir "..\..\target\release"
#endif

; Keep in step with INSTALLER in crates/leungo-cli/src/setup.rs; a test
; there fails when they differ.
#define ApoUrl "https://sourceforge.net/projects/equalizerapo/files/1.4.2/EqualizerAPO-x64-1.4.2.exe/download"
#define ApoSha256 "7403be7427bbe1936a40dded082829b6e217fc4f5990fee5cba501f0ae055afa"

[Setup]
AppId={{94619AC6-8392-4D82-A1F4-512B5BBFD3B7}
AppName=Leungo
AppVersion={#AppVersion}
AppVerName=Leungo {#AppVersion}
AppPublisher=Rizqy EP
AppPublisherURL=https://github.com/rizqyep/leungo
AppSupportURL=https://github.com/rizqyep/leungo/issues
DefaultDirName={autopf}\Leungo
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir=..\..\dist
OutputBaseFilename=Leungo-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\assets\brand\app\leungo.ico
UninstallDisplayIcon={app}\leungo.ico
UninstallDisplayName=Leungo
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=force
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: desktopicon; Description: "Put a Leungo shortcut on the desktop"; Flags: unchecked

[Files]
Source: "{#BinDir}\leungo.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\leungo-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\brand\app\leungo.ico"; DestDir: "{app}"
Source: "..\..\LICENSE"; DestDir: "{app}"
Source: "..\..\README.md"; DestDir: "{app}"

[Icons]
Name: "{autoprograms}\Leungo"; Filename: "{app}\leungo-app.exe"; IconFilename: "{app}\leungo.ico"
Name: "{autodesktop}\Leungo"; Filename: "{app}\leungo-app.exe"; IconFilename: "{app}\leungo.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\leungo-app.exe"; Description: "Open Leungo now"; Flags: nowait postinstall skipifsilent; Check: not ApoJustInstalled

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM leungo.exe"; Flags: runhidden; RunOnceId: "StopLeungo"
Filename: "{app}\leungo.exe"; Parameters: "detach"; Flags: runhidden; RunOnceId: "DetachLeungo"

[Code]
var
  ApoPage: TInputOptionWizardPage;
  DownloadPage: TDownloadWizardPage;
  ApoInstalledNow: Boolean;

function ApoInstalled: Boolean;
var
  Dir: String;
begin
  Result := RegQueryStringValue(HKLM, 'SOFTWARE\EqualizerAPO', 'InstallPath', Dir)
    and FileExists(AddBackslash(Dir) + 'EqualizerAPO.dll');
end;

function ApoJustInstalled: Boolean;
begin
  Result := ApoInstalledNow;
end;

function WantApo: Boolean;
begin
  Result := (ApoPage <> nil) and ApoPage.Values[0] and not WizardSilent;
end;

procedure InitializeWizard;
begin
  if not ApoInstalled then
  begin
    ApoPage := CreateInputOptionPage(wpSelectTasks,
      'One more free program',
      'Leungo uses Equalizer APO to change the sound on Windows.',
      'Setup can download Equalizer APO (about 12 MB) and open its installer. Windows will ask for permission.' + #13#10#13#10 +
      'When its device window appears, tick the device you listen on, such as your headphones or speakers, then click OK. Restart Windows when setup asks, and Leungo opens by itself.',
      False, False);
    ApoPage.Add('Download and install Equalizer APO');
    ApoPage.Values[0] := True;
  end;
  DownloadPage := CreateDownloadPage(SetupMessage(msgWizardPreparing), 'Downloading Equalizer APO', nil);
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if (CurPageID = wpReady) and WantApo then
  begin
    DownloadPage.Clear;
    DownloadPage.Add('{#ApoUrl}', 'EqualizerAPO-setup.exe', '{#ApoSha256}');
    DownloadPage.Show;
    try
      try
        DownloadPage.Download;
      except
        if SuppressibleMsgBox('Equalizer APO could not be downloaded: ' + GetExceptionMessage + #13#10#13#10 +
            'Continue installing Leungo? It will offer Equalizer APO again when you open it.',
            mbError, MB_YESNO, IDYES) = IDNO then
          Result := False
        else
          ApoPage.Values[0] := False;
      end;
    finally
      DownloadPage.Hide;
    end;
  end;
end;

procedure InstallApo;
var
  Code: Integer;
  RunOnce: String;
begin
  RunOnce := 'Software\Microsoft\Windows\CurrentVersion\RunOnce';
  { written first: the Equalizer APO installer may restart Windows itself }
  RegWriteStringValue(HKCU, RunOnce, 'Leungo', '"' + ExpandConstant('{app}\leungo-app.exe') + '"');
  WizardForm.StatusLabel.Caption := 'Waiting for the Equalizer APO installer...';
  if not ShellExec('', ExpandConstant('{tmp}\EqualizerAPO-setup.exe'), '', '', SW_SHOWNORMAL, ewWaitUntilTerminated, Code) then
    Code := -1;
  ApoInstalledNow := ApoInstalled;
  if not ApoInstalledNow then
  begin
    RegDeleteValue(HKCU, RunOnce, 'Leungo');
    SuppressibleMsgBox('Equalizer APO was not installed. Leungo will offer it again when you open it.',
      mbInformation, MB_OK, IDOK);
  end;
end;

function NeedRestart: Boolean;
begin
  Result := ApoInstalledNow;
end;

procedure AddToPath;
var
  Path, Dir: String;
begin
  Dir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Path := '';
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') > 0 then
    Exit;
  if (Path <> '') and (Copy(Path, Length(Path), 1) <> ';') then
    Path := Path + ';';
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path + Dir);
end;

procedure RemoveFromPath;
var
  Path, Dir: String;
  P: Integer;
begin
  Dir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Exit;
  Path := ';' + Path + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Path));
  if P = 0 then
    Exit;
  Delete(Path, P, Length(Dir) + 1);
  Path := Copy(Path, 2, Length(Path) - 2);
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    AddToPath;
    if WantApo then
      InstallApo;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath;
end;
