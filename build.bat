@echo off
rem Build helper: sets up the MSVC environment, then runs cargo.
rem Usage:  build.bat          (debug)
rem         build.bat --release
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
cargo build %*
