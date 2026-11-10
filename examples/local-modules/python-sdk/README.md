# python-sdk: custom nodes with the Python SDK

The same kind of local module as [`../echo`](../echo), written with the SDK in
[`sdk/python/regulus.py`](../../../sdk/python/regulus.py) instead of by hand: the SDK
answers the protocol (docs/03-modules.md §6) and you write one function per node type.
Standard library only, Python 3.9 or newer.

| Node       | Input        | Outputs                          | Field                                  |
|------------|--------------|----------------------------------|----------------------------------------|
| `template` | `in` text@1  | `out` text@1                     | `pattern` (`code`, `{value}` is the text) |
| `lines`    | `in` text@1  | `out` text@1 per line, `count` json@1 | `strip` (`select`: `yes`/`no`)    |

```python
import regulus as rg

@rg.node("template", name="Template",
         inputs=[rg.then_in("in", "text@1")],
         outputs=[rg.then_out("out", "text@1")],
         fields=[rg.field("pattern", "code", default="{value}", language="python")])
def template(ctx, port, value):
    ctx.emit("out", ctx.config["pattern"].format(value=value))

rg.run(version="0.1.0")
```

## Use it in a project

`regulus node init <project> --id <id>` writes a starter `tools/<id>/run.py`, copies the
SDK beside it and adds the `[[modules.local]]` entry. To use this example instead, copy
`run.py` and `sdk/python/regulus.py` into `tools/sdk/` of your project and add:

```toml
[[modules.local]]
id = "sdk"
command = ["python3", "tools/sdk/run.py"]
# env = { REGULUS_MAX_WORKERS = "1" }   # one event at a time, in arrival order
```

Then trust the project: opening a folder never runs its code.

```bash
regulus project open . --trust
regulus module ls          # <project>/sdk@0.1.0   idle
```

Nodes go in a profile with `module_id = "<project>/sdk"`. Changing `run.py` needs the
project closed and opened again (or the core restarted) to load the new manifest.

## Try it by hand

From the repository, the example finds the SDK in `sdk/python/` by itself:

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"manifest","params":{}}' | python3 run.py
```

## What the SDK does and does not do

- `print` and the stdout of child processes go to stderr, which the core logs; stdin
  reads as empty. Only the SDK writes protocol lines.
- Events run on `max_workers` threads (4 by default; `run(max_workers=…)` or
  `REGULUS_MAX_WORKERS`), so a slow event of one execution does not hold the others.
  Handlers that share state must lock it themselves.
- `rg.InvalidInput` answers `-32602`, any other exception `-32000` with the traceback on
  stderr; the process keeps running.
- Only `emit` is offered back to the core: no store, checkpoint or child workflows yet,
  and no per-node `node/start`/`node/stop`.
- It is not a sandbox: the code runs with your permissions, in a project you trust.

Tests: `python3 -m unittest discover -s sdk/python/tests -v`. Design notes and limits:
`docs/18-custom-nodes.md`.
