## Prerequisites

Install NASM `winget -e --id NASM.NASM` and add C:\Users\user\AppData\Local\bin\NASM to your PATH.

## Caveats

On Windows, the screen recorder may produce blank output. This is because hardware composited surfaces and cursors
appear black. To fix this, implement support for media capture in qbt/src/pal/windows.rs. It was not done yet because
most containers don't use GPU compositing.