<#
    Build the VBNote installer.

    Three steps, and it checks for what each one needs before starting rather
    than failing part way through:

      1. cargo build --release          the emulator
      2. PyInstaller                    the setup wizard, as one .exe, so that
                                        the person installing VBNote does not
                                        need Python
      3. ISCC                           the installer itself

    Prerequisites, neither of which this script installs for you:

      pip install -r wizard\requirements.txt pyinstaller
      Inno Setup 6, from https://jrsoftware.org/isdl.php

    The result is dist\VBNote-<version>-setup.exe.
#>
[CmdletBinding()]
param(
    # Skip the Rust build, for when only the wizard or the script changed.
    [switch]$SkipEmulator
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Need($what, $test, $hint) {
    if (-not (& $test)) {
        Write-Error "$what is needed and was not found.`n  $hint"
    }
}

Need 'cargo' { Get-Command cargo -ErrorAction SilentlyContinue } `
     'Install Rust from https://rustup.rs'
Need 'PyInstaller' { python -m PyInstaller --version 2>$null } `
     'pip install pyinstaller'
$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Error "Inno Setup 6 is needed and was not found.`n  https://jrsoftware.org/isdl.php"
}

# --- 1. the emulator -------------------------------------------------------
if (-not $SkipEmulator) {
    Write-Host 'Building the emulator...' -ForegroundColor Cyan
    cargo build --release
    if ($LASTEXITCODE -ne 0) { Write-Error 'the emulator did not build' }
}
if (-not (Test-Path 'target\release\vbnote.exe')) {
    Write-Error 'target\release\vbnote.exe is missing'
}

# --- 2. the wizard ---------------------------------------------------------
# One directory rather than one file: a windowed one-file build unpacks itself
# to a temporary folder on every run, which is slow and trips some antivirus.
Write-Host 'Freezing the setup wizard...' -ForegroundColor Cyan
if (Test-Path 'dist\wizard') { Remove-Item -Recurse -Force 'dist\wizard' }
python -m PyInstaller `
    --noconfirm --clean --windowed `
    --name 'VBNote Setup' `
    --distpath 'dist\pyinstaller' `
    --workpath 'build\pyinstaller' `
    --specpath 'build' `
    --paths . `
    --hidden-import wizard.flashdisk `
    --hidden-import wizard.provision `
    'wizard\wizard.py'
if ($LASTEXITCODE -ne 0) { Write-Error 'the wizard did not freeze' }

New-Item -ItemType Directory -Force -Path 'dist' | Out-Null
Move-Item 'dist\pyinstaller\VBNote Setup' 'dist\wizard'
Remove-Item -Recurse -Force 'dist\pyinstaller'

# --- 3. the installer ------------------------------------------------------
Write-Host 'Building the installer...' -ForegroundColor Cyan
& $iscc 'installer\VBNote.iss'
if ($LASTEXITCODE -ne 0) { Write-Error 'the installer did not build' }

Get-ChildItem 'dist\*setup.exe' | ForEach-Object {
    Write-Host ("`nReady: {0} ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)) -ForegroundColor Green
}
