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

    Continuous integration runs this on a clean Windows runner on every push,
    which is the only way a build needing three separate toolchains stays
    working.
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
# Absolute: PyInstaller resolves a relative --version-file against --specpath,
# not against the working directory, and looks for it under build\ instead.
$versionFile = (Resolve-Path (Join-Path 'installer' 'version.txt')).Path
python -m PyInstaller `
    --noconfirm --clean --windowed `
    --name 'VBNote Setup' `
    --version-file $versionFile `
    --distpath 'dist\pyinstaller' `
    --workpath 'build\pyinstaller' `
    --specpath 'build' `
    --paths . `
    --hidden-import wizard.flashdisk `
    --hidden-import wizard.provision `
    --hidden-import wizard.wizard `
    'vbnote_setup.py'
if ($LASTEXITCODE -ne 0) { Write-Error 'the wizard did not freeze' }

New-Item -ItemType Directory -Force -Path 'dist' | Out-Null
Move-Item 'dist\pyinstaller\VBNote Setup' 'dist\wizard'
Remove-Item -Recurse -Force 'dist\pyinstaller'

# Prove the frozen wizard can reach its own modules before wrapping an
# installer around it. It once shipped unable to: frozen from the wrong
# script, it ran with no parent package and died on its own imports, and
# nothing in the build noticed because everything else about it was fine.
Write-Host 'Checking the frozen wizard starts...' -ForegroundColor Cyan
$wizardExe = 'dist\wizard\VBNote Setup.exe'
if (-not (Test-Path $wizardExe)) { Write-Error "$wizardExe was not built" }
$check = Start-Process -FilePath $wizardExe -ArgumentList '--selftest' `
    -Wait -PassThru -NoNewWindow
if ($check.ExitCode -ne 0) {
    Write-Error "the frozen wizard did not start (exit $($check.ExitCode))"
}

# --- 3. NVDA's controller client -------------------------------------------
# Bundled so VBNote speaks in the user's own screen reader voice instead of
# talking over it in a different one.
#
# Fetched here rather than by the CI workflow so that a local build ships what
# CI ships. When the two were separate they disagreed about the filename --
# the workflow wrote nvdaControllerClient.dll, the installer script asked for
# nvdaControllerClient64.dll -- and 1.0 went out with no client at all.
#
# The x64 build, because it is loaded into vbnote.exe, which is 64-bit. The
# 32-bit one could not be loaded even if it were shipped.
$nvdaVersion = '2024.4.2'
$nvdaDll     = 'nvdaControllerClient.dll'
$nvdaLicence = 'installer\nvda-controllerclient-license.txt'
$nvdaSha     = '0853530a19746f8748994f234ed33589ac255badee41daf82aba47934b5235fb'

if (-not (Test-Path $nvdaDll) -or -not (Test-Path $nvdaLicence)) {
    Write-Host "Fetching NVDA's controller client $nvdaVersion..." -ForegroundColor Cyan
    $url = "https://download.nvaccess.org/releases/$nvdaVersion/nvda_${nvdaVersion}_controllerClient.zip"
    $zip = Join-Path $env:TEMP "nvda-controllerclient-$nvdaVersion.zip"
    $out = Join-Path $env:TEMP "nvda-controllerclient-$nvdaVersion"
    if (-not (Test-Path $zip)) { Invoke-WebRequest -Uri $url -OutFile $zip }
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    Expand-Archive $zip -DestinationPath $out
    Copy-Item (Join-Path $out 'x64\nvdaControllerClient.dll') $nvdaDll -Force
    Copy-Item (Join-Path $out 'license.txt') $nvdaLicence -Force
}

# Checked every time, not only after a fetch: this file is loaded into the
# emulator's own process, and a stale or altered copy left lying beside the
# checkout would otherwise be packaged without a word.
$got = (Get-FileHash $nvdaDll -Algorithm SHA256).Hash.ToLower()
if ($got -ne $nvdaSha) {
    Write-Error ("$nvdaDll is not NVDA's $nvdaVersion controller client.`n" +
                 "  expected $nvdaSha`n" +
                 "  found    $got`n" +
                 "  Delete it and build again to fetch a fresh one.")
}
Write-Host "  $nvdaDll $((Get-Item $nvdaDll).Length) bytes, hash as expected"

# --- 4. the installer ------------------------------------------------------
Write-Host 'Building the installer...' -ForegroundColor Cyan
$log = & $iscc 'installer\VBNote.iss'
$log | ForEach-Object { Write-Host $_ }
if ($LASTEXITCODE -ne 0) { Write-Error 'the installer did not build' }

# What went in, checked against what was meant to. Inno reports a successful
# compile whether or not a given file was included, so "it built" is not the
# same as "it is in there" -- which is exactly how 1.0 shipped without a
# screen reader client and nothing anywhere said so.
foreach ($needed in @('vbnote.exe', 'VBNote Setup.exe', $nvdaDll)) {
    if (-not ($log -match [regex]::Escape($needed))) {
        Write-Error "the installer was built without $needed"
    }
}

Get-ChildItem 'dist\*setup.exe' | ForEach-Object {
    Write-Host ("`nReady: {0} ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)) -ForegroundColor Green
}
