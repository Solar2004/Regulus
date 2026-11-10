# Adding a module to Regulus (a local process module)

A local module is a program the core runs as a process and talks to over stdin/stdout:
JSON-RPC 2.0, one JSON object per line. Any language works, and adding one needs no rebuild
(a module compiled into the host, in Rust, does: it is a crate registered in
`crates/regulus-cli/src/modules.rs`; this guide is not about that). This server never writes
files or runs code itself: you write the files in one of the two ways below, then use the
tools of this server to check what the core makes of them. **Trusting the project's code is
the user's decision**, made with one command that is deliberately not a tool here.

## Steps

1. Pick the project: a folder with a `regulus.toml` (`project_list` shows the open ones; the
   user opens one with `regulus --home <home> project open <folder>`). The module lives in it,
   e.g. `<project folder>/tools/<id>/`.
2. Write three things (next section tells how): the manifest `tools/<id>/regulus-module.toml`,
   the program `tools/<id>/run.py` (or any language), and an entry in the project's
   `regulus.toml`:

   ```toml
   [[modules.local]]
   id = "<id>"                                  # a plain name; listed as <project>/<id>
   command = ["python3", "tools/<id>/run.py"]   # a program with `/` is relative to the project
   # cwd = "tools/<id>"   env = { KEY = "value" }   version = "0.1.0"
   # manifest = "tools/<id>/regulus-module.toml"  # read this file instead of asking the process
   ```

3. **Ask the user to trust and (re)open the project.** Opening a folder never runs its code;
   until the user trusts it, the module is listed as `blocked` and no process starts.
   `[[modules.local]]` entries are read when the project opens, so after editing:

   ```
   regulus --home <home> project close <project id>
   regulus --home <home> project open <project folder> --trust
   ```

4. Check with the tools of this server:
   - `module_list`: `<project>/<id>@<version>` must be `idle` or `active`. `blocked` = the
     project is not trusted (yet); `broken` = the process failed, and the state's reason has
     the last lines of its stderr; `incompatible` = its `requires_proto` does not fit.
   - `module_manifest {id}`: the node types, ports and fields as the core read them.
   - `profile_save` with nodes that use it (`"module_id": "<project>/<id>@<version>"`), then
     `profile_check`. The core refuses only errors: ports or wires that do not exist,
     capabilities or kinds that do not match, duplicate ids. It does **not** refuse a node
     whose module is missing or blocked: it saves the profile and says so in `diagnostics`
     (info `node.module_not_installed`, warning `port.required_unwired`), and at run time the
     node does not start. Read the diagnostics; do not take a successful save for a working module.
   - `workflow_start` returns the execution; its `disabled` list and `notes` name the nodes
     that were left off and why; `event_list` shows a `failed` event ("node ... is not running")
     for values sent to them. `workflow_send` then `event_list` proves the module works.

## Writing the files

**A. With your own file tools** (a coding agent): create the three files in the project folder.

**B. With only this server**: run a workflow of the `files` module that writes them. Save
this profile after replacing `<project folder>` (an absolute path, the `dir` of each node) and
`<id>`, start it, and send the three texts to its inputs; `event_list` then shows each node's
`written` path. `regulus.toml` is appended to, never replaced. With `allow_outside` unset (as
here) the `files` module keeps these writes under `dir`; that is how this profile is
configured, not something this server guarantees: any profile you save can set another `dir`
or `allow_outside = "yes"`.

```json
{{author_profile}}
```

`workflow_send {execution_id, port: "manifest", value: "<manifest text>"}`, then `program`
and `project` (the `[[modules.local]]` block above). Stop the execution afterwards.
(`templates/regulus-code.toml` and its agent are the other way: apply it and give the
authoring task to its agent.)

## Manifest (`regulus-module.toml`)

```toml
[module]
name = "My module"
requires_proto = "^1"      # the core sets the id; a local module may not declare `methods`

[[node]]
type = "upper"
name = "Upper"

  [[node.port]]
  id = "in"
  capability = "text@1"    # the type of the value that travels; wires must match
  direction = "input"      # input | output
  kind = "then"            # then = events on the bus; uses = a service link
  cardinality = "many"     # optional: how many wires an input accepts (one | many)

  [[node.port]]
  id = "out"
  capability = "text@1"
  direction = "output"
  kind = "then"

  [[node.field]]           # optional settings shown in the editor, read from node.config
  id = "suffix"
  type = "text"
  default = "!"
```

Study a real one with `module_manifest {id: "regulus/hello"}` (or any id from `module_list`).

## Process protocol

The core writes requests, the module answers on stdout (id echoed); every message is one line.

| Core sends | Params | Answer |
|---|---|---|
| `manifest` | `{protocol, project_id, id}` | the manifest as a JSON object, or its TOML text |
| `start` | `{protocol, module_id, project_id, root}` | `null` (optional: `-32601` is fine) |
| `call` | `{method, params}` | `null`, or an error. `method` is `node/start`, `node/event`, `node/stop`, `node/child` or `node/hook` and `params` a `NodeCall` `{execution_id, node: {id, type, config, ...}, args}` |
| `stop` | `{}` | `null`, then stdin closes |

For `node/event`, `args` is `{port, capability, value, from, event}`. To put a value on a
"then" output, write a notification (no `id`):
`{"jsonrpc":"2.0","method":"emit","params":{"execution_id":"..","node_id":"..","port":"out","value":".."}}`.
A node without lifecycle answers `-32601` to `node/start` / `node/stop`.
The process gets a clean environment (only `PATH`, `HOME`, `LANG`, `TERM` plus the entry's
`env`); secrets and the core token never reach it. Log to stderr only.

## Python skeleton (stdlib only)

```python
#!/usr/bin/env python3
import json, pathlib, sys

MANIFEST = pathlib.Path(__file__).with_name("regulus-module.toml")

def send(message):
    message["jsonrpc"] = "2.0"
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def handle(method, params):
    if method == "manifest":
        return MANIFEST.read_text()
    if method in ("start", "stop"):
        return None
    if method == "call":
        inner, call = params["method"], params["params"]
        if inner == "node/event":
            value = call["args"]["value"]
            send({"method": "emit", "params": {
                "execution_id": call["execution_id"], "node_id": call["node"]["id"],
                "port": "out", "value": value.upper()}})
            return None
        raise LookupError(inner)          # node/start, node/stop...: no lifecycle
    raise LookupError(method)

for line in sys.stdin:
    if not line.strip():
        continue
    request = json.loads(line)
    if "id" not in request:
        continue
    try:
        send({"id": request["id"], "result": handle(request["method"], request.get("params") or {})})
    except LookupError as error:
        send({"id": request["id"], "error": {"code": -32601, "message": f"unknown method {error}"}})
    except Exception as error:
        print(repr(error), file=sys.stderr, flush=True)
        send({"id": request["id"], "error": {"code": -32000, "message": repr(error)}})
    if request["method"] == "stop":
        break
```

Complete example in the repository: `examples/local-modules/echo/`. Protocol reference:
`docs/03-modules.md` section 6; projects: `docs/11-composition.md` section 13.

## Profile shape (for `profile_save`)

A profile has workflows; a workflow has nodes and wires. A workflow's inputs and outputs are
its `ports`, reached from inner wires as node id `self` (see the profile above, or
`profile_get` of any saved one). `module_id` is `author/module` or `author/module@version`;
a node's `config` holds the values of the fields its manifest declares.
