# Regulus Rust helper

[`regulus.rs`](regulus.rs) is one file that turns a Rust program into a Regulus **local
module**: a separate process, declared in a project's `regulus.toml`, that the core starts
once the project is trusted and talks to over stdin and stdout (JSON-RPC 2.0, one object per
line, [`docs/03-modules.md`](../../docs/03-modules.md) §6). It needs only `serde_json` and
the standard library. It is not a crate to install: it is copied into your node's crate, so
nothing links against Regulus and no Regulus checkout is needed.

## Get it

```bash
regulus node init <project> --id caps --lang rust   # writes tools/caps/, src/regulus.rs included
regulus project trust <project>                     # building runs the project's code
regulus node build <project> --id caps              # cargo build --release
regulus project open <project>                      # after a rebuild: regulus project reload <project-id> --module caps
```

The `regulus` binary carries this file, so `node init` works from a downloaded release. To
use it in a crate of your own, copy `regulus.rs` beside your `main.rs` and depend on
`serde_json = "1"`. [`examples/local-modules/rust-stdio`](../../examples/local-modules/rust-stdio)
is exactly what `node init --lang rust` writes.

## A node

```rust
mod regulus;

use regulus::{Context, Error, Module};
use serde_json::Value;

fn main() {
    Module::new(include_str!("../regulus-module.toml"))
        .node("caps", caps)
        .run();
}

/// A value arrived on `in`: put it on `out`, upper-cased, after the `prefix` field.
fn caps(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::invalid("text@1 must be a string"))?;
    let prefix = ctx.config("prefix").as_str().unwrap_or("");
    ctx.emit("out", format!("{prefix}{}", text.to_uppercase()))
}
```

The node types, their ports and fields are declared in `regulus-module.toml`, which the
program sends as its manifest. The helper does not read it: a call for a node type with no
handler is answered `InvalidParams`.

## API

| Item | What it does |
|---|---|
| `Module::new(manifest)` | The answer to `manifest`: the text of `regulus-module.toml`. |
| `.node(type, handler)` | Serves `type` with `handler(ctx, port, value) -> Result<(), Error>`. Panics if `type` is registered twice. |
| `.workers(n)` | Events handled at once (default 4, or `REGULUS_MAX_WORKERS`). `1` handles them one at a time, in arrival order. |
| `.max_line_bytes(n)` | Longest line written (default 16 MiB, or `REGULUS_MAX_LINE_BYTES`); match the core's `[local_modules] max_line_bytes`. |
| `.run()` | Serves stdin and stdout until `stop` or the end of stdin. A bad `REGULUS_*` value ends the process with status 2 and says why on stderr. |
| `.serve(input, output)` | The same over any `BufRead` and `Write`, for tests. |
| `Context` | `execution_id`, `node_id`, `node_type`, `node` (as the core sent it, secrets resolved), `event` (the `node/event` arguments without `value`), `project_id` and `root` (from `start`). |
| `ctx.config(id)` | The node's field `id`, `Null` when unset. The core does not apply field defaults. |
| `ctx.emit(port, value)` | Writes the `emit` notification now, so it reaches the core before the call's answer. Errors, writing nothing, if `port` is not one of the node's "then" outputs (when the core sent the node's ports) or the line would be too long. |
| `Error::invalid(msg)` | `-32602`, `InvalidParams` in the core. |
| `Error::failed(msg)` | `-32000`, `Failed` in the core. `?` turns any `std::error::Error` into this. |

A panic in a handler is caught: its call is answered `Failed` with the panic message, the
message also goes to stderr, and the worker and the module go on. `node/start`,
`node/stop`, `node/child` and `node/hook` are answered `-32601` (no lifecycle per node).

## Concurrency and state

The core sends the events of one execution one at a time and waits for each answer, so
within an execution they stay in order. Events of different executions run in parallel on
the workers, so a slow one does not hold the others. One process serves every node of its
types in every execution of the project: each call gets its own `Context`, but state a
handler keeps elsewhere (a `static`, a captured `Arc<Mutex<_>>`) is shared by all of them
and by the workers at once. Key it by `(ctx.execution_id, ctx.node_id)` when it belongs to
one node of one execution.

`stop` is answered at once and nothing more is read. The core then closes stdin and sends
`SIGTERM`, so an event still running ends with the process. When the input just ends (as
in a test of `serve`), the events already read run to the end and are answered before
`serve` returns.

## Limits

- **Stdout and stdin belong to the protocol.** Log with `eprintln!`: the core logs stderr
  and shows its last lines when the module breaks. Never `print!` or `println!`: a whole
  line is skipped as noise, a partial one breaks the next message. Give a child process
  `Stdio::null()` or `Stdio::piped()` for its stdin and stdout. The helper does not redirect
  them for you (that needs `unsafe` file-descriptor calls it avoids).
- Only `emit` of the module context: no store, checkpoints, child executions or hooks. A
  node that needs all of `ModuleCtx` is a static module compiled into `regulus`.
- With `panic = "abort"` a panic cannot be caught: the process ends and the module shows as
  broken.
- It is not a sandbox: the program runs with the user's permissions, only in a trusted
  project, with the environment the core lets through (`env_passthrough` and the entry's
  `env`).
- The core does not check which copy of the helper a node has. To update a node, copy the
  new `regulus.rs` over its `src/regulus.rs` and build again.

## Tests

The helper's tests ([`tests.rs`](tests.rs)) run in-memory and are compiled into the CLI's
end-to-end test, which also builds a node with it, runs it under a real core and checks
that the example matches what `node init` writes:

```bash
cargo test -p regulus-cli --test rust_node          # all of it
cargo test -p regulus-cli --test rust_node sdk::    # the helper alone
```
