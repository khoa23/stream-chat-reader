@echo off
rem Builds ReadComments.exe. Requires Rust: stable-x86_64-pc-windows-gnu
cd /d "%~dp0"
cargo build --release --locked || exit /b 1
copy /y target\release\ReadComments.exe ReadComments.exe >nul
echo Built ReadComments.exe
