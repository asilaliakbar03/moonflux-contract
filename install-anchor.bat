@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Preview\VC\Auxiliary\Build\vcvars64.bat"
cargo install --git https://github.com/coral-xyz/anchor anchor-cli --locked
