## Prerequisites

**NASM:** `winget -e --id NASM.NASM` and add $Env:USERPROFILE\AppData\Local\bin\NASM to your PATH.

**VP9 Support:**

Install Visual Studio 2022 including the following components required by env-libvpx-sys:
- ATL
- Clang

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\Llvm\x64\bin"
```

Install vcpkg, then use vcpkg to install libvpx:

```powershell
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
vcpkg install libvpx:x64-windows
$env:VPX_LIB_DIR = "${pwd}\installed\x64-windows\lib"
$env:VPX_INCLUDE_DIR = "${pwd}\installed\x64-windows\include"
$env:VPX_VERSION = "1.16.0" # use vcpkg list to confirm this version
```

Finally, run the server:

```powershell
cargo run -- serve 1234
```

Get https://github.com/cline/cline branch dpc/computer-use, and:

```powershell
bun install
bun build:sdk
cd app/cli
$Env:CLINE_COMPUTER_USE_PORT=1234
$Env:CLINE_HUB_PORT=5555
bun run dev
```

Then, you must use the Anthropic provider (*not* merely Anthropic models through the Cline provider, because those lack
the computer-use beta header.)

## Caveats

On Windows, the screen recorder may produce blank output. This is because hardware composited surfaces and cursors
appear black. To fix this, implement support for media capture in qbt/src/pal/windows. It was not done yet because
most containers don't use GPU compositing.

## TODO

- Computer use: mouse jumping for clicks and animation for moves, drags
- Computer use: keys, typing
- Render clicks and typing into the video feed
- MCP wrapper