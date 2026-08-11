; VBNote installer.
;
; Built by `installer\build.ps1`, which compiles the emulator, freezes the
; setup wizard, and then runs this. See that script for what it expects.
;
; The person this is for has owned a BrailleNote or VoiceNote, watched it die,
; and wants it back. They are very likely blind. So: no options to choose, no
; components to tick, no README to skip past, and two Start menu entries whose
; names say what they do rather than what they are.

#define Name "VBNote"
#define Version "0.1.0"
#define Publisher "VBNote project"
#define Url "https://github.com/highenergymagic/VBNote"
#define Exe "vbnote.exe"
#define SetupExe "VBNote Setup.exe"

[Setup]
AppId={{7B2F1C4E-9A3D-4E62-9C1B-5D4A8F0E2B77}
AppName={#Name}
AppVersion={#Version}
AppPublisher={#Publisher}
AppPublisherURL={#Url}
AppSupportURL={#Url}/issues
DefaultDirName={autopf}\{#Name}
DefaultGroupName={#Name}
; Nothing here is worth a decision from the user, so do not ask for one.
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
DisableWelcomePage=no
AllowNoIcons=no
OutputDir=..\dist
OutputBaseFilename=VBNote-{#Version}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Per-user by default: no administrator prompt, which is one fewer thing to
; get past and one fewer dialog a screen reader has to fight with.
PrivilegesRequiredOverridesAllowed=dialog
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=terms.txt
UninstallDisplayName={#Name}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\target\release\{#Exe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\wizard\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\README.md"; DestDir: "{app}"; DestName: "README.txt"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion isreadme
; NVDA's controller client, if it was put beside the build. Without it VBNote
; still runs and still speaks, through the system voice instead.
Source: "..\nvdaControllerClient64.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
; Setup first: it is what a new installation needs, and the machine will not
; start without it having been run once.
Name: "{group}\Set up VBNote"; Filename: "{app}\{#SetupExe}"; Comment: "Build your machine from your own firmware. Do this first."
Name: "{group}\VBNote"; Filename: "{app}\{#Exe}"; Comment: "Start your machine"
Name: "{group}\VBNote settings"; Filename: "notepad.exe"; Parameters: """{%USERPROFILE}\.VBNote\VBNote.ini"""; Comment: "Edit the settings file"
Name: "{group}\Uninstall VBNote"; Filename: "{uninstallexe}"
Name: "{autodesktop}\VBNote"; Filename: "{app}\{#Exe}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Put VBNote on the desktop"; GroupDescription: "Shortcuts:"

[Run]
; Offer to go straight into setting the machine up, because an installation
; that has not been through the wizard cannot start.
Filename: "{app}\{#SetupExe}"; Description: "Set up my machine now"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
; The machine itself is the user's data -- their documents are on that flash
; disk -- so it is never removed. Uninstalling leaves ~\.VBNote alone.

[Messages]
WelcomeLabel2=This will install [name/ver].%n%nVBNote is an emulator of the VoiceNote QT and BrailleNote mPower. It is not a HumanWare product and is not supported by them.%n%nAfter installing, run "Set up VBNote" once. It builds your machine from firmware files you supply from a machine you own; VBNote does not include them and cannot obtain them.
