## Running qwanban

Run the server:

```powershell
cargo run -- serve 1234 5678
```

Get https://github.com/cline/cline branch dpc/computer-use, and:

```powershell
bun install
bun build:sdk
cd apps/cli
$Env:CLINE_COMPUTER_USE_PORT=1234
$Env:CLINE_HUB_PORT=5555
$Env:CLINE_COMPUTER_USER_MODEL = "claude-sonnet-5"
bun run dev
```

Then, you must use the Anthropic provider (*not* merely Anthropic models through the Cline provider, because those lack
the computer-use beta header.)

## TODO

- Computer use: mouse jumping for clicks but animation for moves, drags
- MCP wrapper
