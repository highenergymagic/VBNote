@echo off
setlocal

rem Boot KeySoft 8.0 and let you type at it.
rem
rem Everything except the firmware is made on the first run, so there is
rem nothing to set up. Double-click this, or run it from a terminal.

cd /d "%~dp0"

set EBOOT=roms\EBOOT.bin
set NK=roms\NK.bin
set CARD=work\card.img
set EEPROM=work\SerialNumber.bin
set EXE=target\release\vbnote.exe

echo VBNote - VoiceNote QT mPower, KeySoft 8.0
echo.

rem ---------------------------------------------------------------- firmware
if exist "%EBOOT%" if exist "%NK%" goto firmwareok

echo I cannot find the firmware.
echo.
echo This emulator does not ship it. HumanWare own it, and you supply your own
echo copy. Put both of these in the roms folder beside this script:
echo.
echo     %EBOOT%
echo     %NK%
echo.
echo Then run this again.
echo.
pause
exit /b 1

:firmwareok

rem ---------------------------------------------------------------- the build
if exist "%EXE%" goto builtok

echo Building. This takes a minute or two the first time, and prints a lot.
echo.
cargo build --release
if errorlevel 1 (
    echo.
    echo The build failed. It needs a recent Rust toolchain; see the README.
    echo.
    pause
    exit /b 1
)
echo.

:builtok

if not exist work mkdir work

rem ------------------------------------------------------------- the SD card
rem This is the Flash Disk. The emulator makes the image if it is not there,
rem and Windows CE partitions and formats it during the first boot -- which
rem takes longer than KeySoft waits, so the first boot always ends with it
rem saying the flash disk is unavailable. Expected, and only once.
if exist "%CARD%" goto secondboot

echo There is no SD card image yet, so this run will make one and format it.
echo.
echo KeySoft will say the flash disk is unavailable and stop. That is normal
echo and it only happens once: formatting is still running when it asks. Let
echo it get that far, close the window, and run this again.
echo.
goto go

:secondboot
echo The card is ready, so this should come up properly.
echo.

:go
echo A window will open. Put focus on it and type - the machine answers in
echo speech, and the window is only there to catch keystrokes. There is
echo nothing to look at.
echo.
echo Expect about a minute and a half of silence, then music, then it asks
echo which language to use. Press Enter for English. Close the window to stop.
echo.

rem --cpu-mhz is the flag that decides how long any of this takes. It sets the
rem clock the guest's timers run against, so a figure near what this emulator
rem actually retires makes a guest second last about a real second. Leave it
rem out and the default of 1200 assumes a core far faster than this one, so
rem every delay loop in the firmware burns four times the cycles waiting.
rem
rem Measured here: first sound at 4.8 G cycles against 11.3 G, and the language
rem prompt in about 145 seconds against 400.
rem
rem If your machine is much faster or slower than this one, run it by hand with
rem --free-run --cycles 4000000000, time it, and pass the millions of cycles a
rem second you get.

"%EXE%" "%EBOOT%" --flash --nk "%NK%" ^
    --cpu-mhz 63 ^
    --sd-card "%CARD%" ^
    --serial-eeprom "%EEPROM%" ^
    --keyboard ^
    --status work\status

echo.
echo Stopped.
pause
