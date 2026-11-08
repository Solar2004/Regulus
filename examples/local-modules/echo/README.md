# echo: a local module in Python

A Regulus module that runs as a process of its own, written in plain Python 3 (stdlib
only). It brings two node types:

| Node    | Input        | Output        | Does                     |
|---------|--------------|---------------|--------------------------|
| `echo`  | `in` text@1  | `out` text@1  | puts on `out` what came  |
| `shout` | `in` text@1  | `out` text@1  | the same, in upper case  |

The protocol is in `docs/03-modules.md` §6: JSON-RPC 2.0 over stdin/stdout, one object
per line. The core asks `manifest`, `start`, `call` and `stop`; the module sends `emit`
notifications to put values on its "then" outputs. Anything written to stderr goes to the
core's log, and its last lines explain why the module broke if it dies.

## Declare it in a project

Copy this folder into your project (say `tools/echo/`) and add to its `regulus.toml`:

```toml
[[modules.local]]
id = "echo"                           # listed as <project>/echo
command = ["python3", "tools/echo/echo.py"]
# cwd = "tools/echo"                  # where it runs; the project folder by default
# env = { ECHO_DEBUG = "1" }          # added to the environment
# version = "0.1.0"                   # defaults to the one in its manifest
# manifest = "tools/echo/regulus-module.toml"  # read this instead of asking the process
```

Then open the project and trust it. Opening a folder never runs its code: without trust,
its local modules are listed as `blocked` and no process starts.

```bash
regulus project open . --trust     # or later: regulus project trust .
regulus module ls                  # <project>/echo@0.1.0   idle
regulus project trust --remove .   # stops it again; it shows as blocked
```

The process gets a clean environment: only `[local_modules] env_passthrough` of
`config.toml` (`PATH`, `HOME`, `LANG`, `TERM` by default) plus its own `env`.

Use it in a profile like any module, with `module_id = "<project>/echo@0.1.0"` on the
node, and veto it with `[modules] disable = ["<project>/echo"]`. Closing the project stops
the process: `stop`, then SIGTERM, then SIGKILL after `[local_modules] stop_grace_ms` of
`config.toml`.

## Try it by hand

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"manifest","params":{}}' | python3 echo.py
```
