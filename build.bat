@echo off
rem MKDS Native Builder: build.bat "path\to\your game.nds" ["output folder"]
setlocal
if "%~1"=="" (
  echo Usage: build.bat "C:\path\to\your\game.nds" ["output folder"]
  echo Default output folder: %~dp0builds\mkds
  exit /b 1
)
if not exist "%~1" (
  echo ROM not found: %~1
  exit /b 1
)
where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is not installed. Install it from https://rustup.rs, then open a NEW terminal window and try again.
  exit /b 1
)
set "OUT=%~2"
if "%OUT%"=="" set "OUT=%~dp0builds\mkds"
set "BUILDER=%~dp01_build_tool\target\release\mkds_builder.exe"
echo Preparing the builder (the first time this takes a few minutes)...
cargo build --release --locked --manifest-path "%~dp01_build_tool\Cargo.toml"
if errorlevel 1 (
  echo Could not build the builder. See the messages above.
  exit /b 1
)
"%BUILDER%" "%~1" --build --out "%OUT%"
if errorlevel 1 (
  echo.
  echo Build failed. See the messages above, or the Troubleshooting section of README.md.
  exit /b 1
)
echo.
echo Done! Start the game with:  play.bat   (or "%OUT%\play.exe")
