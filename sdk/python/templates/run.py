#!/usr/bin/env python3
"""__NAME__: a Regulus custom node in Python, written by `regulus node init`.

The core runs this file as a local module of the project (`[[modules.local]]` in
`regulus.toml`) once the project is trusted, and talks to it over stdin/stdout through
`regulus.py`, the SDK beside it (standard library only). `print` is safe: the SDK moves
it to stderr, which the core logs. This is not a sandbox: it runs with your permissions.

Change the node below, add more with `@rg.node(...)`, and reload the module to use them
(`regulus project reload <project> --module __ID__`, or Reload in the app): the core keeps
the running version until the new one starts. More in docs/18-custom-nodes.md.
"""
import regulus as rg


@rg.node(
    "__ID__",
    name="__NAME__",
    inputs=[rg.then_in("in", "text@1")],
    outputs=[rg.then_out("out", "text@1")],
    fields=[rg.field("prefix", "text", default="", label="Prefix")],
)
def handle(ctx, port, value):
    """A value arrived on `port` ("in"): put it on "out", upper-cased, after the prefix."""
    if not isinstance(value, str):
        raise rg.InvalidInput("text@1 must be a string")
    ctx.emit("out", ctx.config["prefix"] + value.upper())


if __name__ == "__main__":
    rg.run(version="0.1.0")
