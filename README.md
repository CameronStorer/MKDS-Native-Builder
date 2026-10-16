# MKDS Native Builder

Turn **your own copy of MKDS** into a **native program**, built on your own Windows, Linux, or macOS computer.

This repository contains **no game**: no ROM, no game code, no graphics, sounds or other game files. You supply your own `.nds`
file; the builder reads it on your machine, translates the game's machine code into Rust source code, compiles it and gives you a
`play.exe` on Windows or `play` on Linux/macOS. Nothing from the game ever leaves your computer.

---

## Quick start (Windows 10/11, 64-bit)

**1. Install Rust** (one time)
- Download and run `rustup-init.exe` from **https://rustup.rs** and accept the defaults.
- If it asks to install the **Visual Studio C++ Build Tools**, say yes (Rust needs them on Windows).
- Close and reopen any terminal windows afterwards.

**2. Get this repository**
- Click **Code -> Download ZIP** on GitHub and unzip it (for example to `C:\MKDS Native Builder`), **or** run
  `git clone https://github.com/CameronStorer/mkds-native-builder`.

**3. Build** (one command)
- Open the folder, click the address bar, type `cmd` and press Enter. Then run:
  ```
  build.bat "C:\path\to\your\game.nds"
  ```
- The first build takes **about 10-20 minutes** (it downloads the Rust libraries it needs and compiles everything). You will see
  progress in five steps. Rebuilds are much faster (often under a minute).

**4. Play**
  ```
  play.bat
  ```
  (or double-click `builds\mkds\play.exe`).

That's it.

**Or use the menu:** double-click **`mkds.bat`**. It lets you build the game (drag your `.nds` file into the window when it
asks), play it, and change the controls, all from one place.

---

## Quick start (Linux and macOS)

Build on the operating system where you will play; these scripts do not cross-compile.

**1. Install prerequisites**

On Debian/Ubuntu Linux:

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config curl ca-certificates libasound2-dev libudev-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev libx11-xcb-dev \
  libwayland-dev libxkbcommon-dev
```

Other Linux distributions need equivalent C compiler, pkg-config, ALSA, X11, Wayland, and xkbcommon development packages.
Playing requires a graphical desktop and a working audio device. WSL2 requires WSLg for the game window;
if ALSA has no default audio device, use `--mute` or install `libasound2-plugins` and configure ALSA to use your
WSLg PulseAudio server. To test audio without changing your system defaults, save this as `alsa-wsl.conf` in
your output folder:

```text
</usr/share/alsa/alsa.conf>
pcm.!default { type pulse }
ctl.!default { type pulse }
```

Then launch from that folder with `ALSA_CONFIG_PATH="$PWD/alsa-wsl.conf" ./play` (WSLg supplies `PULSE_SERVER`).

On macOS, install Apple's command line tools:

```sh
xcode-select --install
```

On either OS, install the current stable Rust toolchain from **https://rustup.rs**, then reopen your terminal.
Use the native Rust toolchain for your processor (including Apple Silicon). macOS support is untested.

**2. Build and play**

Download or clone this repository, open a terminal in it, and run:

```sh
sh build.sh "/path/to/your/game.nds"
sh play.sh
```

In a Git checkout the scripts can also be run as `./build.sh` and `./play.sh`. Scripts work from other directories
too; relative ROM/output paths are relative to your terminal's current directory. Quote paths containing spaces.

For a custom output directory:

```sh
sh build.sh "/path/to/your/game.nds" "/path/to/empty/output"
"/path/to/empty/output/play" --stats
```

`play.sh` starts the default `builds/mkds/play` and forwards all game arguments. Native outputs are OS-specific;
use separate output folders when sharing a checkout between Windows and WSL. The shell builder keeps its own
compiler artifacts under `1_build_tool/target/<OS>-<architecture>`.

## Controls

| Keyboard | DS button | In the game |
|---|---|---|
| **Z** | A | Accelerate / confirm |
| **X** | B | Brake / back |
| **S** | R | Hop / drift |
| **A** | L | Use item |
| **C** / **V** | X / Y | |
| **Arrow keys** | D-pad | Steer / menus |
| **Enter** | Start | Pause / start |
| **Right Shift** | Select | |
| **Left mouse button** (on the lower screen) | Touch screen | |
| **Esc** | | Quit |
| **F11** | | Fullscreen on/off |

**Game controllers** (Xbox, PlayStation, Switch Pro and most others) work too: plug one in before starting the game.

| Controller | DS button |
|---|---|
| **A** (bottom face button) or **right trigger** | A (accelerate) |
| **B** (right face button) or **left trigger** | B (brake) |
| **Y** / **X** (top / left face buttons) | X / Y |
| **Left / right bumper** | L (item) / R (drift) |
| **D-pad** or **left stick** | D-pad |
| **Start** / **Back (View)** | Start / Select |

### Changing the controls

Double-click `mkds.bat` and choose **Change controls**: pick a DS button, press the new key, then **Save and go back**.

Or edit the controls file in any text editor. It is created with the keys above the first time the game starts:

- Windows: `%APPDATA%\MKDS Native Builder\controls.txt`
- macOS: `~/Library/Application Support/MKDS Native Builder/controls.txt`
- Linux: `~/.config/mkds-native-builder/controls.txt`

Each line is `Button = Key`, for example `L = Q`. Give a button several keys with commas (`A = Z, Space`) or none (`Select =`).
Buttons: `A B X Y L R Start Select Up Down Left Right Quit`. Key names: `A`-`Z`, `Key0`-`Key9`, `F1`-`F15`, `Up Down Left
Right`, `Space Enter Escape Tab Backspace Delete Insert Home End PageUp PageDown`, `LeftShift RightShift LeftCtrl RightCtrl
LeftAlt RightAlt LeftSuper RightSuper`, `Apostrophe Backquote Backslash Comma Equal LeftBracket RightBracket Minus Period
Semicolon Slash`, `NumPad0`-`NumPad9 NumPadDot NumPadSlash NumPadAsterisk NumPadMinus NumPadPlus NumPadEnter`,
`CapsLock NumLock ScrollLock Pause Menu`. Changes apply the next time the game starts, and the file survives rebuilds.
If the file has a mistake, the game says which line; delete the file to get the default controls back.
To use a different file once: `play.bat --controls "C:\path\to\controls.txt"`.

Controller buttons have their own lines, starting with `Pad`: for example `Pad L = LB, RT`. Names are by position, so they
work for any controller: `South East North West` (face buttons; South is Xbox A / PlayStation Cross), `LB RB LT RT`,
`Start Select`, `DPadUp DPadDown DPadLeft DPadRight`, `StickUp StickDown StickLeft StickRight` (left stick),
`LeftStick RightStick` (pressed in) and `Guide`. Files without `Pad` lines use the controller layout above.

---

## Display

The 3D graphics and both screens are drawn by your **graphics card** (Direct3D 12 or Vulkan on Windows, Vulkan on Linux,
Metal on macOS). **By default the 3D is drawn as sharp as your window allows** (up to 4x the DS's 256 x 192); menus, map
and HUD keep the DS's own pixels, top screen above the bottom one. The window can be resized and made fullscreen (F11).

**Want the exact original look?** Set `Scale = 1` in `settings.txt`, or start once with `play.bat --scale 1`. A fixed
`Scale = 2` suits 1080p monitors, `3` or `4` 1440p / 4K. The game plays exactly the same at every setting.

Double-click `mkds.bat` and choose **Display settings**, or edit `settings.txt` (in the same folder as `controls.txt`):

| Setting | Values | What it does |
|---|---|---|
| `Renderer` | `GPU` (default), `Software` | `Software` draws everything on the CPU, pixel for pixel like the console (can drop frames on slower PCs) |
| `Scale` | `Auto` (default), `1`-`8` | 3D sharpness as a multiple of 256 x 192. `Auto` picks the sharpest that fits the window, up to 4x; `1` is the original look |
| `Filter` | `Sharp` (default), `Smooth` | How the screens look when stretched to the window |
| `Layout` | `Stacked` (default), `SideBySide` | Top screen above the bottom one, or next to it for wide monitors |
| `Fullscreen` | `No` (default), `Yes` | Start in fullscreen |

Every option is explained inside `settings.txt` itself. For one run, `play.bat --renderer software` or `play.bat --scale 4`
override the file. Graphics cards without compute
shaders (very old ones) automatically fall back to drawing on the CPU.

### Texture packs

The GPU renderer can save every texture the game uses and load replacements, for example sharper hand-made or upscaled
versions:

1. In `settings.txt`, set `DumpTextures = C:\MKDS textures\dump` and play: each texture is saved once as `<id>.png`.
2. Edit or upscale the PNGs you want. A replacement must be the original size times a whole number from 1 to 8 (for example
   a 64 x 32 texture can become 256 x 128). Keep the file names.
3. Put the replacements in a folder and set `TexturePack = C:\MKDS textures\pack` (and `DumpTextures =` to stop dumping).

A replacement with the wrong size is ignored with a message naming the file. Dumped textures come from your copy of the game:
do not share them.

---

## What you need

- **Your own ROM** of MKDS, USA version: game code `AMCE`, revision 0. Make it from a cartridge you own. Other versions are refused
  with a clear message (they are not supported yet).
- Windows 10 or 11 (64-bit), desktop Linux, or macOS (untested); about **2 GB** free disk space and **8 GB RAM** recommended.
- Rust (step 1). An internet connection for the **first** build only.

---

## What the build produces

Everything goes into `builds\mkds\` (ignored by git, so it can never be uploaded by accident):

| Item | What it is |
|---|---|
| `play.exe` | Starts the game after checking that none of your game files were changed or are missing |
| `game.exe` | The compiled game |
| `assets\` | The game's files, unpacked from your ROM |
| `source\` | The Rust code generated from your ROM: you can read it and rebuild it yourself |
| `manifest.json` | Fingerprints (SHA-256) of your ROM, the files, the generated code and the program |

On Linux/macOS the executables are named `play` and `game`, with the same bundle layout. Keep their executable
permissions when moving a bundle. A bundle must run on a compatible OS/architecture with the native libraries installed.

**Please do not share `assets\`, `source\` or the built game** - they are made from your copy of the game.

---

## Options

Run `build.bat "game.nds" "C:\some\other\empty\folder"` to build somewhere else. Advanced users can call the builder directly:

```
1_build_tool\target\release\mkds_builder.exe "game.nds" --inspect                 check the ROM only
1_build_tool\target\release\mkds_builder.exe "game.nds" --rip --out FOLDER        only unpack the game files
1_build_tool\target\release\mkds_builder.exe "game.nds" --build --out FOLDER      full build
```

Useful build options: `--offline` (no network, once everything is downloaded), `--jobs N` (how many CPU cores to use),
`--no-cache` (fresh build), `--clear-cache` (delete the build cache), `--no-source` (don't keep `source\`).
The default cache is `%LOCALAPPDATA%\mkds_builder` on Windows, `${XDG_CACHE_HOME:-~/.cache}/mkds_builder` on Linux,
and `~/Library/Caches/mkds_builder` on macOS. Override it with `--cache-dir FOLDER`.
On Unix, call `1_build_tool/target/$(uname -s)-$(uname -m)/release/mkds_builder` for the same advanced options.

Game options (after `play.exe` or `play`): `--mute`, `--stats` (prints speed and frame counters once a second).

---

## Troubleshooting

| Message | What to do |
|---|---|
| `Rust is not installed` / `cargo is not recognized` | Do step 1, then open a **new** terminal window. |
| `Output must be empty` | The folder already has a build. Delete `builds\mkds` or give a different output folder. |
| `Unsupported ROM ... expected AMCE revision 0` | Only the USA version, revision 0, is supported right now. |
| `This ROM differs from the reference` | Your file is not a clean copy (modified or a different dump). |
| `Asset changed: ...` | A game file in `builds\mkds\assets` was edited or deleted. Build again. |
| Linker / `link.exe` errors | Install the Visual Studio C++ Build Tools (step 1) and try again. |
| The build fails with a Cargo error | Run it again with an internet connection; if it still fails, open an issue with the message. |

---

## How it works (short version)

1. **Check** - your ROM is identified by its fingerprint; unsupported or changed files are refused.
2. **Unpack** - the game's files are copied out of the ROM into `assets\` with a fingerprint list.
3. **Translate** - every function of the game's two processors (ARM9 and ARM7) is translated from ARM machine code into Rust. The
   list of where functions start (`4_docs/FUNCTION_BOUNDARIES.csv`, addresses only) guides this; the code itself comes from your ROM.
4. **Compile** - Rust compiles the generated code together with a small runtime that provides the DS hardware (screens, sound,
   input, timers) on your PC.
5. **Package** - `play.exe`, `game.exe`, `assets\`, `source\` and the fingerprints are placed in the output folder.

No instruction is emulated at run time: the shipped game contains no interpreter. The 3D graphics and the final screens are drawn
by your graphics card at a higher resolution, and the sound channels are mixed straight at your audio device's rate. The 2D
layers (backgrounds and sprites) are still decoded on the CPU at the DS's resolution, as the game's code drives them.

Research notes and progress: **https://docs.cameronstorer.com/MKDS-Decomp-Docs/**

## Status

- Linux: builder, recompiler/generator, and native-host tests pass on Debian 13 x86-64 under WSL2. A full ROM build
  and 600-frame WSLg run with audio and scripted input completed successfully, reaching nickname confirmation.
  Recoverable ALSA underruns no longer terminate the game. Full Linux races, physical keyboard/mouse input,
  audio quality, and performance across other Linux desktops have not been validated. WSLg emitted nonfatal
  Wayland decoration/cleanup warnings during the smoke test.
- macOS: native build path and setup instructions provided; not tested on a Mac.
- Windows: menus, Grand Prix races (Mushroom, Flower, Shell cups), Mission 1-1, Battle mode, full races through the results screen;
  real-time speed and clean audio on a 6-core PC. GPU renderer (block 2): checked against the CPU renderer on 549 recorded frames
  (99.99% identical pixels; the differences are single edge pixels) and played at 60 frames per second with sound on an RTX 2060.
  The GPU renderer and controllers have not been tested on Linux or macOS yet.
- Known issues: occasional stutter, the race position badge can show the wrong number, rare visual glitches, the in-game keyboard
  may repeat keys.

### Developer checks

```sh
cargo test --locked --manifest-path 1_build_tool/Cargo.toml
cargo test --locked --manifest-path 2_native_game/Cargo.toml --features gen
cargo test --locked --manifest-path 2_native_game/native/Cargo.toml
sh tests/shell_scripts.sh  # Linux/macOS wrapper checks; no ROM needed
```

Some ROM-dependent tests skip unless their documented environment variables are set; the ignored native-window
test is Windows-only. For a bounded smoke test of an actual Unix bundle, run `./play --frames 300 --stats`
from its output folder (add `--mute` if no audio device is available).

## Legal

This project is not affiliated with or endorsed by Nintendo. It contains no copyrighted game data and does not download any. You need
a ROM you made from a cartridge you own. Do not distribute ROMs, unpacked game files, generated source or built games.
