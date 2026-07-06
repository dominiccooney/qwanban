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

Finally, `cargo build`.

## Caveats

On Windows, the screen recorder may produce blank output. This is because hardware composited surfaces and cursors
appear black. To fix this, implement support for media capture in qbt/src/pal/windows.rs. It was not done yet because
most containers don't use GPU compositing.

## TODO

- Computer use: clicks, typing
- Render clicks and typing into the video feed
- MCP wrapper