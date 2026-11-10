#!/usr/bin/env python3
"""Custom Regulus nodes written with the Python SDK (sdk/python/regulus.py).

Two node types:

- `template` formats each text that arrives with the `pattern` field (`{value}` is the
  text) and puts the result on `out`.
- `lines` splits each text into lines and puts every non-empty line on `out`, one value
  each, and how many there were on `count`.

`regulus node init` copies `regulus.py` beside the `run.py` it writes. This example uses
the SDK in place when no copy sits beside it, so it runs from the repository too.
"""
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
if not (HERE / "regulus.py").exists():
    sys.path.insert(0, str(HERE.parents[2] / "sdk" / "python"))

import regulus as rg  # noqa: E402


@rg.node(
    "template",
    name="Template",
    inputs=[rg.then_in("in", "text@1")],
    outputs=[rg.then_out("out", "text@1")],
    fields=[
        rg.field(
            "pattern", "code", default="{value}", language="python",
            label="Pattern", description="Python format string; {value} is the text.",
        ),
    ],
)
def template(ctx, port, value):
    if not isinstance(value, str):
        raise rg.InvalidInput("text@1 must be a string")
    try:
        ctx.emit("out", ctx.config["pattern"].format(value=value))
    except (AttributeError, KeyError, IndexError, ValueError) as error:
        raise rg.InvalidInput(f"bad pattern: {error!r}") from None


@rg.node(
    "lines",
    name="Lines",
    inputs=[rg.then_in("in", "text@1")],
    outputs=[rg.then_out("out", "text@1"), rg.then_out("count", "json@1")],
    fields=[rg.field("strip", "select", default="yes", options=["yes", "no"])],
)
def lines(ctx, port, value):
    if not isinstance(value, str):
        raise rg.InvalidInput("text@1 must be a string")
    strip = ctx.config["strip"] == "yes"
    sent = 0
    for line in value.splitlines():
        text = line.strip() if strip else line
        if text.strip():
            ctx.emit("out", text)
            sent += 1
    ctx.emit("count", sent)


if __name__ == "__main__":
    rg.run(name="Python SDK example", version="0.1.0")
