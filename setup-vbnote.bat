@echo off
rem The VBNote setup wizard: next, next, next, finish.
rem
rem It builds a machine in %%USERPROFILE%%\.VBNote from firmware you supply.
rem The same job without a window:
rem   python -m wizard --eboot EBOOT.bin --nk NK.bin
cd /d "%~dp0"
python -m wizard.wizard %*
