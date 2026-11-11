# rust-stdio: a custom node in Rust

A Regulus node written in Rust that runs as a **separate process** (a *local module*) and
talks to the core over stdin/stdout: JSON-RPC 2.0, one object per line
([`docs/03-modules.md`](../../../docs/03-modules.md) §6). It depends only on `serde_json`.
It brings one node type, `rust-stdio`, that puts its input on `out` in upper case after a
`prefix` field.

`src/main.rs` is the node: a handler and one line that registers it. `src/regulus.rs` is the
helper that speaks the protocol, a copy of [`sdk/rust/regulus.rs`](../../../sdk/rust/regulus.rs)
(what it does and its limits: [`sdk/rust/README.md`](../../../sdk/rust/README.md)).

Every file here is exactly what `regulus node init --lang rust` writes for the id
`rust-stdio` (a test keeps them equal), so this folder is also the template.

## Use it in a project

```bash
regulus node init <project> --id caps --lang rust    # writes tools/caps/ and registers it
regulus project trust <project>                      # building runs the project's code
regulus node build <project> --id caps               # cargo build --release
regulus project open <project>                       # after a rebuild: regulus project reload <project-id> --module caps
regulus module ls                                    # <project>/caps@0.1.0   idle
```

Use it on a node with `module_id = "<project>/caps@0.1.0"` and `type = "caps"`.
`regulus node build` builds into `tools/caps/target`, whatever `CARGO_TARGET_DIR` is, and
honors `$CARGO`. Until it is built, the module shows as `broken`; until the project is
trusted, as `blocked`. Opening a folder never runs its code.

To use this folder as it is, copy it to `<project>/tools/rust-stdio/` and add to
`regulus.toml`:

```toml
[[modules.local]]
id = "rust-stdio"
command = ["tools/rust-stdio/target/release/node-rust-stdio"]
```

## Try it by hand

```bash
cargo build --release
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"call","params":{"method":"node/event","params":{"execution_id":"e1","node":{"id":"n","type":"rust-stdio","config":{"prefix":"> "}},"args":{"port":"in","capability":"text@1","value":"hello"}}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"stop"}' | target/release/node-rust-stdio
```

It writes an `emit` notification with `"> HELLO"` and then the result of call 1; the answer
to `stop` may come first, since `stop` is answered at once while the event runs on a worker.
Its own tests run with `cargo test`.

## What this is, and what it is not

| | Static module | Local module (this) | Python SDK node |
|---|---|---|---|
| Written as | a crate in `crates/modules/` plus one line in the registry | any program that speaks the protocol; in Rust, with `sdk/rust/regulus.rs` | `sdk/python/regulus.py` |
| Needs a new `regulus` binary | **yes**: the registry is compiled in | **no** | **no** |
| Runs | inside the core, with all of `ModuleCtx` | as a process, with `emit` only | as a process, with `emit` only |
| Needs a trusted project | no | **yes** | **yes** |
| Install command | none: edit the sources and rebuild (`docs/03-modules.md` §5) | `regulus node init` and `regulus node build` | `regulus node init` |

- **No sandbox.** The process runs with your permissions; trust and an empty environment
  (`env_passthrough`) are what protect you.
- **Building is not loading.** `regulus node build` only runs `cargo`; the core starts the
  program the next time the project is opened.
- There is no `regulus module install` and no dynamic library or WASM loading; a node that
  needs all of `ModuleCtx` or sits on the hot path is a static module.
