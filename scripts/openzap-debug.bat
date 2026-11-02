@echo off
rem Starts OpenZap with a diagnostic log (openzap-debug.log next to openzap.exe).
rem Stream URLs in the log are masked.
start "" "%~dp0openzap.exe" --debug %*
