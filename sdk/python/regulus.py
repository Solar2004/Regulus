"""Regulus custom nodes in Python: one file, standard library only.

A project's local module (``[[modules.local]]`` in ``regulus.toml``) is a process that
speaks JSON-RPC 2.0 with the core over stdin/stdout, one object per line
(docs/03-modules.md §6). This module hides that protocol: declare node types with
:func:`node`, then call :func:`run`::

    import regulus as rg

    @rg.node("template", name="Template",
             inputs=[rg.then_in("in", "text@1")],
             outputs=[rg.then_out("out", "text@1")],
             fields=[rg.field("pattern", "code", default="{value}", language="python")])
    def template(ctx, port, value):
        ctx.emit("out", ctx.config["pattern"].format(value=value))

    rg.run()

What it does for you:

- answers ``manifest`` with the node types declared here, ``start`` and ``stop``;
- turns ``call`` → ``node/event`` into ``handler(ctx, port, value)`` and ``ctx.emit`` into
  the ``emit`` notification, written before the call's answer so the core puts it on the
  bus before the call returns;
- runs events on a bounded pool of worker threads (``max_workers``), so a slow event of
  one execution does not hold the events of the others;
- moves what the handler prints (and what its child processes write to their stdout) to
  stderr, which the core logs: only protocol messages reach the real stdout, and stdin is
  only read by the SDK;
- answers errors with the codes the core maps: :class:`InvalidInput` → ``-32602``
  (``InvalidParams``), any other exception (``sys.exit`` included) → ``-32000``
  (``Failed``) with the traceback on stderr. The process keeps running;
- refuses, as a handler error, a value the core could not read: not JSON, text that is
  not UTF-8, or a line longer than the core takes (``max_line_bytes``).

Ports that depend on the node's config (protocol 1.17): ``then_in``/``then_out`` with
``when={"field": "mode", "equals": "split"}`` exist only when the field says so, and with
``repeat="cases"`` (a ``"list"`` field) one port per item, ``{item}`` in the id replaced.
The core computes each node's ports from its config (``module.shape``); a handler sees the
ports of its node, so ``ctx.emit`` to a repeated port works.

:func:`inspect` reads a node file without running it (``ast``, literal arguments only): what
the core's ``source.check`` uses to report syntax errors and the declared node types.

It is not a sandbox: the process runs with the user's permissions, only in a project the
user trusts. Copy this file beside your ``run.py`` (``regulus node init`` does it).
"""

import ast
import copy
import json
import os
import queue
import sys
import threading
import traceback

__all__ = [
    "Context",
    "InvalidInput",
    "NodeError",
    "field",
    "inspect",
    "node",
    "run",
    "then_in",
    "then_out",
]

#: This SDK's own version, kept here and nowhere else (the app has its own; they match
#: in 0.2.1). The core warns when a project's copy is not the one it carries.
__version__ = "0.2.1"

#: The core protocol this SDK speaks (docs/03-modules.md §6).
REQUIRES_PROTO = "^1"
#: What a module with dynamic ports needs: an older core would drop them silently and the
#: node would load without them, so such a core refuses it instead (`incompatible`).
REQUIRES_DYNAMIC_PROTO = "^1.17"
#: Events handled at once when neither ``run(max_workers=…)`` nor the environment says.
DEFAULT_MAX_WORKERS = 4
#: Environment variable that sets ``max_workers`` (from ``env`` in ``regulus.toml``).
MAX_WORKERS_ENV = "REGULUS_MAX_WORKERS"
#: The longest line the core reads from a module: ``[local_modules] max_line_bytes``, whose
#: default is this. A longer line gets the process killed, so the SDK refuses to write it.
DEFAULT_MAX_LINE_BYTES = 16 * 1024 * 1024
#: Environment variable to set when the core's ``max_line_bytes`` is not the default.
MAX_LINE_BYTES_ENV = "REGULUS_MAX_LINE_BYTES"

JSONRPC = "2.0"
PARSE_ERROR = -32700
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
FAILED = -32000

# What the core sends.
MANIFEST = "manifest"
START = "start"
CALL = "call"
STOP = "stop"
NODE_EVENT = "node/event"
# What the process sends.
EMIT = "emit"

INPUT = "input"
OUTPUT = "output"
THEN = "then"
CARDINALITIES = ("one", "many")
# Keys of a field the SDK writes itself; ``**extra`` may not repeat them.
FIELD_KEYS = ("id", "type", "default", "options")
# The keys a ``when`` condition takes.
WHEN_KEYS = ("field", "equals", "in")


class NodeError(Exception):
    """An error the handler raises on purpose; the core marks the call ``Failed``."""

    code = FAILED


class InvalidInput(NodeError):
    """The value or the configuration is not acceptable; the core sees ``InvalidParams``."""

    code = INVALID_PARAMS


def _encode(message):
    """``message`` as UTF-8 JSON bytes, the way the core reads it.

    ``ensure_ascii=False`` plus a strict encode refuses lone surrogates (``os.fsdecode``,
    ``surrogateescape``): Python would escape them, and the core's parser drops such a line.
    """
    return json.dumps(message, allow_nan=False, ensure_ascii=False).encode("utf-8")


def _json_value(name, value):
    """``value`` itself, if it can travel as JSON; a ``ValueError`` naming it if not."""
    try:
        _encode(value)
    except (TypeError, ValueError) as error:  # UnicodeEncodeError is a ValueError
        raise ValueError(f"{name} must be JSON: {error}") from None
    return value


def _printable(text):
    """``text`` with anything that is not UTF-8 written as an escape, safe to send."""
    return text.encode("utf-8", "backslashreplace").decode("utf-8")


def _when(owner, when):
    """``when`` checked and copied: a dict with a ``field`` and ``equals`` or ``in``."""
    if not isinstance(when, dict) or not isinstance(when.get("field"), str) or not when["field"]:
        raise ValueError(f"{owner}: when must be a dict naming a field, e.g. "
                         "{'field': 'mode', 'equals': 'split'}")
    unknown = sorted(set(when) - set(WHEN_KEYS))
    if unknown:
        raise ValueError(f"{owner}: when takes {', '.join(WHEN_KEYS)}, not {', '.join(unknown)}")
    copied = dict(when)
    if "in" in copied:
        if not isinstance(copied["in"], (list, tuple)):
            raise ValueError(f"{owner}: when's `in` must be a list")
        copied["in"] = list(copied["in"])
    return _json_value(f"{owner} when", copied)


def _port(direction, port_id, capability, name, required, cardinality, lazy, when, repeat):
    if not isinstance(port_id, str) or not port_id:
        raise ValueError("a port needs a non-empty string id")
    if not isinstance(capability, str) or not capability:
        raise ValueError(f"port `{port_id}` needs a capability, e.g. `text@1`")
    port: dict = {
        "id": port_id,
        "name": name or "",
        "capability": capability,
        "direction": direction,
        "kind": THEN,
    }
    if required:
        port["required"] = True
    if direction == INPUT:
        if cardinality not in CARDINALITIES:
            raise ValueError(f"port `{port_id}`: cardinality is `one` or `many`")
        port["cardinality"] = cardinality
        if lazy:
            port["lazy"] = True
    if when is not None:
        port["when"] = _when(f"port `{port_id}`", when)
    if repeat is not None:
        if not isinstance(repeat, str) or not repeat:
            raise ValueError(f"port `{port_id}`: repeat names a list field")
        port["repeat"] = repeat
    return port


def _dynamic(port):
    """Whether a port depends on the config: it goes in the manifest's ``dynamic_port``."""
    return "when" in port or "repeat" in port


def then_in(port_id, capability, *, name="", required=False, cardinality="many", lazy=False,
            when=None, repeat=None):
    """A "then" input: values arrive on it as ``handler(ctx, port_id, value)``.

    ``cardinality`` is ``"many"`` by default, so several wires can feed it. ``when`` and
    ``repeat`` make it depend on the node's config (see the module docs).
    """
    return _port(INPUT, port_id, capability, name, required, cardinality, lazy, when, repeat)


def then_out(port_id, capability, *, name="", when=None, repeat=None):
    """A "then" output: ``ctx.emit(port_id, value)`` puts a value on it.

    ``when`` and ``repeat`` make it depend on the node's config (see the module docs).
    """
    return _port(OUTPUT, port_id, capability, name, False, None, False, when, repeat)


def field(field_id, field_type="text", *, default=None, options=None, when=None, **extra):
    """An option of the node, drawn by the inspector and read as ``ctx.config[field_id]``.

    ``field_type`` is open (``text``, ``number``, ``select``, ``code``, ``list``…): a client
    that does not know it shows the raw value. ``when`` hides it unless another field says
    so. ``extra`` keys travel with the field as they are (``label``, ``description``,
    ``multiline``, ``language``…) for clients that use them.
    """
    if not isinstance(field_id, str) or not field_id:
        raise ValueError("a field needs a non-empty string id")
    if not isinstance(field_type, str) or not field_type:
        raise ValueError(f"field `{field_id}` needs a type")
    clash = [key for key in extra if key in FIELD_KEYS]
    if clash:
        raise ValueError(f"field `{field_id}`: `{clash[0]}` is not an extra key")
    spec: dict = {"id": field_id, "type": field_type}
    if default is not None:
        spec["default"] = _json_value(f"field `{field_id}` default", default)
    if options is not None:
        spec["options"] = _json_value(f"field `{field_id}` options", list(options))
    if when is not None:
        spec["when"] = _when(f"field `{field_id}`", when)
    for key, value in extra.items():
        spec[key] = _json_value(f"field `{field_id}` `{key}`", value)
    return spec


class _NodeType:
    def __init__(self, entry, handler):
        self.entry = entry
        self.node_type = entry["type"]
        self.handler = handler
        # The ports every node of the type has; a node's own (from the call) win.
        self.inputs = {p["id"] for p in entry["port"] if p["direction"] == INPUT}
        self.outputs = {p["id"] for p in entry["port"] if p["direction"] == OUTPUT}
        self.defaults = {f["id"]: f["default"] for f in entry["field"] if "default" in f}
        self.warned_return = False

    def manifest(self):
        return self.entry


def _ports_of(raw, direction, declared):
    """The ids of a node's ports in ``direction``: those the core sent with the node (its
    shape, dynamic ports included), else the ones the type always has."""
    ports = raw.get("ports")
    if not isinstance(ports, list) or not ports:
        return declared
    return {p.get("id") for p in ports
            if isinstance(p, dict) and p.get("direction") == direction
            and p.get("kind", THEN) == THEN}


# The node types this process declares, in declaration order.
_NODES = {}


def _unique(kind, owner, ids):
    seen = set()
    for item in ids:
        if item in seen:
            raise ValueError(f"{owner}: {kind} `{item}` is declared twice")
        seen.add(item)


def _declare(node_type, *, name=None, inputs=(), outputs=(), fields=()):
    """A node type's manifest entry, checked as :func:`node` checks it (``ValueError``)."""
    if not isinstance(node_type, str) or not node_type:
        raise ValueError("a node type needs a non-empty string")
    inputs, outputs, fields = list(inputs), list(outputs), list(fields)
    owner = f"node `{node_type}`"
    for port in inputs:
        if not isinstance(port, dict) or port.get("direction") != INPUT:
            raise ValueError(f"{owner}: an entry of inputs is not then_in(...)")
    for port in outputs:
        if not isinstance(port, dict) or port.get("direction") != OUTPUT:
            raise ValueError(f"{owner}: an entry of outputs is not then_out(...)")
    for spec in fields:
        if not isinstance(spec, dict) or "id" not in spec:
            raise ValueError(f"{owner}: an entry of fields is not field(...)")
    fixed = [p for p in inputs + outputs if not _dynamic(p)]
    dynamic = [p for p in inputs + outputs if _dynamic(p)]
    # Dynamic ports may share an id (a variant that retypes a port); the core checks them.
    _unique("port", owner, [p["id"] for p in fixed])
    _unique("field", owner, [f["id"] for f in fields])
    entry = {"type": node_type, "name": name or node_type, "port": fixed, "field": fields,
             "managed_ports": True}
    if dynamic:
        entry["dynamic_port"] = dynamic
    return entry


def node(node_type, *, name=None, inputs=(), outputs=(), fields=()):
    """Declares a node type whose "then" inputs are handled by the decorated function.

    The handler is called as ``handler(ctx, port, value)`` for each value that arrives;
    it puts values on its outputs with ``ctx.emit``. What it returns is ignored. Every node
    of the type has the ports its config gives (``managed_ports``).
    """
    if isinstance(node_type, str) and node_type in _NODES:
        raise ValueError(f"node type `{node_type}` is declared twice")
    entry = _declare(node_type, name=name, inputs=inputs, outputs=outputs, fields=fields)

    def register(handler):
        if not callable(handler):
            raise TypeError(f"node `{node_type}`: the handler must be callable")
        _NODES[node_type] = _NodeType(entry, handler)
        return handler

    return register


def manifest(module_name=None, version=None):
    """The manifest the core asks for: the shape of ``regulus-module.toml`` as JSON.

    The core names the module ``<project>/<id>@<version>`` whatever is said here;
    ``version`` is used only when ``regulus.toml`` does not set one.
    """
    dynamic = any("dynamic_port" in n.entry for n in _NODES.values())
    module: dict = {"requires_proto": REQUIRES_DYNAMIC_PROTO if dynamic else REQUIRES_PROTO}
    if module_name:
        module["name"] = module_name
    if version:
        module["id"] = f"local/sdk@{version}"
    return {"module": module, "node": [n.manifest() for n in _NODES.values()]}


class _Wire:
    """The protocol's stdout: whole lines, one writer at a time."""

    def __init__(self, stream, max_line_bytes=DEFAULT_MAX_LINE_BYTES):
        self._stream = stream
        self._lock = threading.Lock()
        self.max_line_bytes = max_line_bytes

    def send(self, message):
        """Writes ``message`` as one line; ``ValueError`` (nothing written) if it cannot go."""
        message["jsonrpc"] = JSONRPC
        line = _encode(message) + b"\n"
        if len(line) > self.max_line_bytes:
            raise ValueError(
                f"the message is {len(line)} bytes; the core reads lines of at most "
                f"{self.max_line_bytes} ({MAX_LINE_BYTES_ENV}, [local_modules] max_line_bytes)")
        with self._lock:
            self._stream.write(line)
            self._stream.flush()


class Context:
    """What a handler sees of one node in one execution.

    - ``config``: the node's settings, over the defaults of its fields.
    - ``execution_id``, ``node_id``, ``node_type``, ``node``: who it is (``node`` is the
      raw node of the profile, with ``secret:<name>`` already swapped by the core).
    - ``event``: the raw ``node/event`` arguments (``capability``, ``from``, ``event``).
    - ``project_id``, ``root``: the project, from ``start``.
    """

    def __init__(self, runtime, call, node_type):
        self._runtime = runtime
        self._type = node_type
        raw = call.get("node") or {}
        self.node = raw
        self.execution_id = call.get("execution_id", "")
        self.node_id = raw.get("id", "")
        self.node_type = node_type.node_type
        self.event = call.get("args") or {}
        # A deep copy: a handler that changes a list or table default changes only its own.
        config = copy.deepcopy(node_type.defaults)
        config.update(raw.get("config") or {})
        self.config = config
        self.project_id = runtime.started.get("project_id")
        self.root = runtime.started.get("root")
        self._outputs = _ports_of(raw, OUTPUT, node_type.outputs)

    def emit(self, port, value=None):
        """Puts ``value`` on the "then" output ``port`` of this node.

        Raises ``ValueError`` if the node type does not declare that output, the value
        cannot travel as JSON (or as UTF-8), or the line would be longer than the core
        reads; nothing is sent then.
        """
        if port not in self._outputs:
            declared = ", ".join(sorted(self._outputs)) or "none"
            raise ValueError(f"`{self.node_type}` has no output `{port}` (outputs: {declared})")
        _json_value(f"the value emitted on `{port}`", value)
        self._runtime.wire.send({"method": EMIT, "params": {
            "execution_id": self.execution_id,
            "node_id": self.node_id,
            "port": port,
            "value": value,
        }})

    def log(self, *parts):
        """Writes a line to stderr, which the core logs as this module's."""
        print(*parts, file=sys.stderr, flush=True)


class _Failure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


class _Runtime:
    def __init__(self, wire, module_name, version, max_workers):
        self.wire = wire
        self.module_name = module_name
        self.version = version
        self.max_workers = max_workers
        self.started = {}
        self.jobs = queue.Queue()
        for index in range(max_workers):
            worker = threading.Thread(target=self._work, name=f"regulus-sdk-{index}", daemon=True)
            worker.start()

    def answer(self, request_id, result=None, error=None):
        if error is None:
            message = {"id": request_id, "result": result}
        else:
            message = {"id": request_id, "error": {
                "code": error["code"], "message": _printable(error["message"])}}
        try:
            self.wire.send(message)
        except ValueError as failure:  # the core could not read it: say so in a line that fits
            self.wire.send({"id": request_id, "error": {"code": FAILED, "message": str(failure)}})

    def _work(self):
        while True:
            request_id, params = self.jobs.get()
            try:
                self.serve(request_id, lambda: self.call(params))
            except BaseException:  # noqa: B036 — a worker never dies: the pool stays whole
                traceback.print_exc(file=sys.stderr)
                sys.stderr.flush()

    def serve(self, request_id, work):
        """Runs ``work`` and answers ``request_id`` with its result or its error."""
        try:
            result = work()
        except _Failure as failure:
            self.answer(request_id, error={"code": failure.code, "message": str(failure)})
        except NodeError as error:
            self.answer(request_id, error={"code": error.code, "message": str(error)})
        except BaseException as error:  # noqa: B036 — a bug or sys.exit() in a handler
            # answers as an error and never ends the module or its worker thread.
            traceback.print_exc(file=sys.stderr)
            sys.stderr.flush()
            message = f"{type(error).__name__}: {error}" if str(error) else type(error).__name__
            self.answer(request_id, error={"code": FAILED, "message": message})
        else:
            self.answer(request_id, result=result)

    def call(self, params):
        method = params.get("method")
        if method != NODE_EVENT:
            # node/start, node/stop, node/child, node/hook: no lifecycle per node yet.
            raise _Failure(METHOD_NOT_FOUND, f"unknown method `{method}`")
        call = params.get("params") or {}
        raw = call.get("node") or {}
        node_type = _NODES.get(raw.get("type"))
        if node_type is None:
            raise _Failure(INVALID_PARAMS, f"no node type `{raw.get('type')}` here")
        event = call.get("args") or {}
        port = event.get("port")
        if port not in _ports_of(raw, INPUT, node_type.inputs):
            raise _Failure(INVALID_PARAMS, f"`{node_type.node_type}` takes nothing on `{port}`")
        ctx = Context(self, call, node_type)
        returned = node_type.handler(ctx, port, event.get("value"))
        if returned is not None and not node_type.warned_return:
            node_type.warned_return = True
            ctx.log(f"regulus: `{node_type.node_type}` returned a value; use ctx.emit to send it")
        return None

    def request(self, request_id, method, params):
        """One request from the core. ``True`` once the process should end."""
        if method == CALL:
            if params.get("method") == NODE_EVENT:
                self.jobs.put((request_id, params))
            else:
                # Answered here, not behind busy workers: the core waits on node/start and
                # node/stop to start or stop an execution.
                self.serve(request_id, lambda: self.call(params))
            return False
        if method == MANIFEST:
            self.serve(request_id, lambda: manifest(self.module_name, self.version))
        elif method == START:
            self.started = dict(params)
            self.serve(request_id, lambda: None)
        elif method == STOP:
            self.serve(request_id, lambda: None)
            return True
        else:
            self.answer(request_id, error={
                "code": METHOD_NOT_FOUND, "message": f"unknown method `{method}`"})
        return False


def _positive(name, value, default):
    if value is None:
        return default
    try:
        number = int(value)
    except (TypeError, ValueError):
        raise ValueError(f"{name} must be a whole number, not {value!r}") from None
    if number < 1:
        raise ValueError(f"{name} must be at least 1, not {number}")
    return number


def _max_workers(explicit):
    value = explicit if explicit is not None else os.environ.get(MAX_WORKERS_ENV)
    return _positive("max_workers", value, DEFAULT_MAX_WORKERS)


def _max_line_bytes():
    value = os.environ.get(MAX_LINE_BYTES_ENV)
    return _positive(MAX_LINE_BYTES_ENV, value, DEFAULT_MAX_LINE_BYTES)


def _take_stdio():
    """The protocol's stdin and stdout, private to the SDK.

    File descriptors 0 and 1 are pointed at ``/dev/null`` and stderr, so ``print``,
    ``input`` and child processes cannot write into the protocol or read from it. The
    duplicated descriptors are not inherited by child processes. Stdin is buffered (one
    ``read`` per byte made a 4 MiB value take seconds).
    """
    sys.stdout.flush()
    proto_in = os.fdopen(os.dup(0), "rb")
    proto_out = os.fdopen(os.dup(1), "wb")
    # Ends a line printed without a newline before run(); the core skips blank lines.
    proto_out.write(b"\n")
    proto_out.flush()
    os.dup2(2, 1)
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)
    sys.stdout = sys.stderr
    sys.stdin = open(os.devnull, encoding="utf-8")
    return proto_in, proto_out


def run(*, name=None, version=None, max_workers=None):
    """Serves the node types declared with :func:`node` until the core stops it.

    - ``name``: the module's display name (the core uses its id otherwise).
    - ``version``: used when ``regulus.toml`` does not set one.
    - ``max_workers``: events handled at once; ``REGULUS_MAX_WORKERS`` or 4 by default.
      Use 1 to handle one event at a time, in arrival order.
    """
    if not _NODES:
        raise RuntimeError("no node types: declare one with @regulus.node(...) before run()")
    workers = _max_workers(max_workers)
    max_line_bytes = _max_line_bytes()
    proto_in, proto_out = _take_stdio()
    runtime = _Runtime(_Wire(proto_out, max_line_bytes), name, version, workers)
    for raw in iter(proto_in.readline, b""):
        line = raw.decode("utf-8", errors="replace").strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError as error:
            print(f"regulus: not JSON on stdin ({error}): {line[:200]}", file=sys.stderr, flush=True)
            continue
        if not isinstance(message, dict) or "method" not in message:
            continue  # an answer to one of our notifications, or noise
        if message.get("id") is None:
            continue  # nothing is notified to us today
        params = message.get("params")
        if not isinstance(params, dict):
            params = {}
        if runtime.request(message["id"], message["method"], params):
            break
    # Stopped or stdin closed: events still running end with the process (daemon threads).
    proto_out.flush()
    sys.stderr.flush()


# --- reading a node file without running it ------------------------------------------

#: The SDK builders the static reader evaluates, with literal arguments only.
_BUILDERS = {"then_in": then_in, "then_out": then_out, "field": field}
#: How deep the static reader follows nested literals and names.
_MAX_DEPTH = 64
#: How many values the static reader builds for one file before it stops: a name used
#: twice stands for its value twice, so `x2 = [x1, x1]` doubles at every line.
_MAX_VALUES = 100_000
#: How many characters of text (and bytes of numbers) those values may hold in all: a
#: long string used many times is that many copies once serialized into the manifest.
_MAX_BYTES = 4 * 1024 * 1024
#: What a builder's result costs beyond its arguments: the keys it adds itself.
_BUILT_BYTES = 128


def _literal_bytes(value):
    """About how much a literal weighs once written out as JSON: text by its length, an
    integer by its decimal digits (a 4300-digit literal is not 8 bytes), anything else 8."""
    if isinstance(value, str):
        return len(value)
    if isinstance(value, int) and not isinstance(value, bool):
        return max(8, abs(value).bit_length() // 3 + 1)
    return 8
#: How much of an expression a diagnostic quotes.
_QUOTE = 80


class _Unreadable(Exception):
    """An expression that cannot be read without running the file, and why (``None``: it
    is not a literal; else what limit of the reader it went past)."""

    def __init__(self, expr, why=None):
        super().__init__(expr)
        self.expr = expr
        self.why = why


def _quote(expr):
    text = " ".join(ast.unparse(expr).split())
    return text if len(text) <= _QUOTE else text[:_QUOTE - 1] + "…"


def inspect(source, filename="run.py"):
    """Reads a node file without running it: its syntax errors and the node types its
    ``@node(...)`` decorators declare, as far as their arguments are literals.

    ``source`` is text or bytes. Nothing of it is imported or executed: it is parsed with
    ``ast``, and only literals, module-level names bound once to a literal, and this SDK's
    own ``then_in``, ``then_out`` and ``field`` are evaluated. Returns a dict with
    ``interpreter``, ``diagnostics`` (``severity``, ``code``, ``message``, and 1-based
    ``line``, ``column``, ``end_line``, ``end_column`` in code points), ``nodes`` (each a
    ``line`` and the manifest entry ``node``) and ``complete`` (every declaration was read).
    """
    data = source.encode("utf-8", "surrogatepass") if isinstance(source, str) else bytes(source)
    result = {"interpreter": "Python " + sys.version.split()[0], "diagnostics": [],
              "nodes": [], "complete": True}
    lines = data.splitlines()

    def column(line, byte_offset):
        # ``ast`` counts UTF-8 bytes; clients count characters.
        if line is None or byte_offset is None or byte_offset < 0:
            return None
        if 1 <= line <= len(lines):
            return len(lines[line - 1][:byte_offset].decode("utf-8", "replace")) + 1
        return byte_offset + 1

    def say(severity, code, message, where=None, whole=True):
        item = {"severity": severity, "code": code, "message": message}
        if where is not None:
            item["line"] = where.lineno
            item["column"] = column(where.lineno, where.col_offset)
            if whole and getattr(where, "end_lineno", None) is not None:
                item["end_line"] = where.end_lineno
                item["end_column"] = column(where.end_lineno, where.end_col_offset)
        result["diagnostics"].append({k: v for k, v in item.items() if v is not None})

    try:
        tree = ast.parse(data, filename=filename)
    except SyntaxError as error:
        item = {"severity": "error", "code": "python.syntax", "message": error.msg or str(error)}
        if error.lineno:
            item["line"] = error.lineno
            if error.offset and error.offset > 0:
                item["column"] = error.offset
            end_line = getattr(error, "end_lineno", None)
            end_offset = getattr(error, "end_offset", None)
            if end_line and end_offset and end_offset > 0 and (end_line, end_offset) > (
                    error.lineno, item.get("column", 0)):
                item["end_line"], item["end_column"] = end_line, end_offset
        result["diagnostics"].append(item)
        result["complete"] = False
        return result
    except (ValueError, RecursionError, MemoryError) as error:
        say("error", "python.syntax", f"the file cannot be parsed: {error}")
        result["complete"] = False
        return result
    # What the parser lets through and loading would refuse (`return` outside a function,
    # a repeated argument, `break` outside a loop…): compiled, never run.
    try:
        compile(tree, filename, "exec", dont_inherit=True)
    except SyntaxError as error:
        # The compiler counts UTF-8 bytes, 1-based.
        item = {"severity": "error", "code": "python.syntax", "message": error.msg or str(error)}
        if error.lineno:
            item["line"] = error.lineno
            item["column"] = column(error.lineno, (error.offset or 1) - 1)
            end_line = getattr(error, "end_lineno", None)
            end_offset = getattr(error, "end_offset", None)
            if end_line and end_offset and end_offset > 0:
                item["end_line"] = end_line
                item["end_column"] = column(end_line, end_offset - 1)
        result["diagnostics"].append({k: v for k, v in item.items() if v is not None})
        result["complete"] = False
        return result
    except (ValueError, RecursionError, MemoryError) as error:
        say("error", "python.syntax", f"the file cannot be compiled: {error}")
        result["complete"] = False
        return result

    # How the file names this SDK, and the module-level names bound once to something.
    modules, names, bound, count = set(), {}, {}, {}
    for stmt in tree.body:
        if isinstance(stmt, ast.Import):
            for alias in stmt.names:
                if alias.name == "regulus":
                    modules.add(alias.asname or "regulus")
        elif isinstance(stmt, ast.ImportFrom) and stmt.module == "regulus" and not stmt.level:
            for alias in stmt.names:
                if alias.name != "*":
                    names[alias.asname or alias.name] = alias.name
        # `node = rg.node`: another name for an SDK function, resolved by name, never run.
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 \
                and isinstance(stmt.targets[0], ast.Name):
            value = stmt.value
            if isinstance(value, ast.Attribute) and isinstance(value.value, ast.Name) \
                    and value.value.id in modules:
                names[stmt.targets[0].id] = value.attr
            elif isinstance(value, ast.Name) and value.id in names:
                names[stmt.targets[0].id] = names[value.id]
        targets = []
        if isinstance(stmt, ast.Assign):
            targets = stmt.targets
        elif isinstance(stmt, (ast.AnnAssign, ast.AugAssign, ast.For, ast.AsyncFor)):
            targets = [stmt.target]
        for target in targets:
            if isinstance(target, ast.Name):
                count[target.id] = count.get(target.id, 0) + 1
                if isinstance(stmt, ast.Assign) or (isinstance(stmt, ast.AnnAssign) and stmt.value):
                    bound[target.id] = stmt.value
    bound = {name: value for name, value in bound.items() if count[name] == 1}

    def sdk(func):
        if isinstance(func, ast.Attribute) and isinstance(func.value, ast.Name) \
                and func.value.id in modules:
            return func.attr
        if isinstance(func, ast.Name):
            return names.get(func.id)
        return None

    # Every value built counts once, and its text by length; a name is read once and then
    # stands for its value's whole cost each time it is used. One budget for the whole
    # file, so neither repeated names nor long strings make the reader work or allocate
    # without bound (before anything is copied or serialized).
    spent = [0, 0]
    read = {}

    def charge(count, size, expr):
        spent[0] += count
        spent[1] += size
        if spent[0] > _MAX_VALUES:
            raise _Unreadable(expr, f"builds more than {_MAX_VALUES} values")
        if spent[1] > _MAX_BYTES:
            raise _Unreadable(expr, f"builds more than {_MAX_BYTES} bytes of values")

    def ev(expr, depth=0):
        if depth > _MAX_DEPTH:
            raise _Unreadable(expr, f"nests more than {_MAX_DEPTH} levels")
        if isinstance(expr, ast.Constant):
            value = expr.value
            if value is None or isinstance(value, (str, int, float, bool)):
                charge(1, _literal_bytes(value), expr)
                return value
            raise _Unreadable(expr)
        charge(1, 0, expr)
        if isinstance(expr, (ast.List, ast.Tuple)):
            if any(isinstance(item, ast.Starred) for item in expr.elts):
                raise _Unreadable(expr)
            return [ev(item, depth + 1) for item in expr.elts]
        if isinstance(expr, ast.Dict):
            out = {}
            for key, value in zip(expr.keys, expr.values):
                if key is None:
                    raise _Unreadable(expr)
                name = ev(key, depth + 1)
                if not isinstance(name, str):
                    raise _Unreadable(key)
                out[name] = ev(value, depth + 1)
            return out
        if isinstance(expr, ast.UnaryOp) and isinstance(expr.op, (ast.USub, ast.UAdd)):
            value = ev(expr.operand, depth + 1)
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return -value if isinstance(expr.op, ast.USub) else value
            raise _Unreadable(expr)
        if isinstance(expr, ast.Name) and expr.id in bound:
            known = read.get(expr.id)
            if known is None:
                before = list(spent)
                try:
                    value = ev(bound[expr.id], depth + 1)
                except _Unreadable as unread:
                    read[expr.id] = unread
                    raise _Unreadable(expr, unread.why) from None  # said where it is used
                known = read[expr.id] = (value, spent[0] - before[0], spent[1] - before[1])
            elif isinstance(known, _Unreadable):
                raise _Unreadable(expr, known.why)
            else:
                charge(known[1], known[2], expr)
            return copy.deepcopy(known[0])
        if isinstance(expr, ast.Call):
            builder = _BUILDERS.get(sdk(expr.func))
            if builder is None or any(isinstance(a, ast.Starred) for a in expr.args):
                raise _Unreadable(expr)
            # Keyword names become keys of the result (`field(**extra)`): they weigh too.
            charge(0, _BUILT_BYTES + sum(len(k.arg) for k in expr.keywords if k.arg), expr)
            args = [ev(a, depth + 1) for a in expr.args]
            kwargs = {}
            for keyword in expr.keywords:
                if keyword.arg is None:
                    raise _Unreadable(keyword.value)
                kwargs[keyword.arg] = ev(keyword.value, depth + 1)
            return builder(*args, **kwargs)
        raise _Unreadable(expr)

    declared = set()
    for stmt in tree.body:
        if not isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for decorator in stmt.decorator_list:
            call = decorator if isinstance(decorator, ast.Call) else None
            if sdk(call.func if call else decorator) != "node":
                continue
            if call is None:
                say("error", "node.declaration",
                    f"`@{_quote(decorator)}` needs a node type: `@{_quote(decorator)}(\"my-type\", "
                    "inputs=[...], outputs=[...])`", decorator)
                result["complete"] = False
                continue
            try:
                if any(isinstance(a, ast.Starred) for a in call.args):
                    raise _Unreadable(call)
                args = [ev(a) for a in call.args]
                kwargs = {}
                for keyword in call.keywords:
                    if keyword.arg is None:
                        raise _Unreadable(keyword.value)
                    kwargs[keyword.arg] = ev(keyword.value)
                entry = _declare(*args, **kwargs)
            except _Unreadable as unread:
                why = f"{unread.why} to read without running it" if unread.why else "is not a literal"
                say("warning", "python.unreadable",
                    f"`{_quote(unread.expr)}` in the @node(...) of `{stmt.name}` {why}, so it "
                    "is only known once the module runs (reload it)", unread.expr)
                result["complete"] = False
                continue
            except (ValueError, TypeError) as error:
                say("error", "node.declaration",
                    f"@node(...) of `{stmt.name}`: {error}; the module would fail to start",
                    decorator)
                result["complete"] = False
                continue
            if entry["type"] in declared:
                say("error", "node.declaration",
                    f"node type `{entry['type']}` is declared twice; the module would fail "
                    "to start", decorator)
                result["complete"] = False
                continue
            declared.add(entry["type"])
            result["nodes"].append({"line": decorator.lineno, "node": entry})
            arguments = stmt.args
            positional = len(arguments.posonlyargs) + len(arguments.args)
            if positional < 3 and arguments.vararg is None:
                say("warning", "python.handler",
                    f"`{stmt.name}` is called as {stmt.name}(ctx, port, value) but takes "
                    f"{positional} argument(s)", stmt, whole=False)
            if isinstance(stmt, ast.AsyncFunctionDef):
                say("warning", "python.handler",
                    f"`{stmt.name}` is async: the SDK calls handlers without awaiting them, "
                    "so its body would never run", stmt, whole=False)
    if not declared and result["complete"]:
        say("warning", "python.no_nodes",
            "the file declares no node: decorate a handler with @rg.node(\"type\", ...)")
    runs = any(isinstance(item, ast.Call) and sdk(item.func) == "run" for item in ast.walk(tree))
    if not runs:
        say("warning", "python.no_run",
            "the file never calls run(): the module would end as soon as it starts")
    return result


def _inspect_main():
    """What the core runs (``python -I -S -c <this file> + "_inspect_main()"``): reads a
    node file on stdin and writes what :func:`inspect` found as one JSON object."""
    data = sys.stdin.buffer.read()
    try:
        result = inspect(data)
    except Exception as error:  # noqa: BLE001 — the core always gets an answer
        result = {"interpreter": "Python " + sys.version.split()[0], "nodes": [],
                  "complete": False, "diagnostics": [{
                      "severity": "warning", "code": "check.unavailable",
                      "message": f"the checker failed: {type(error).__name__}: {error}"}]}
    text = json.dumps(result, ensure_ascii=False, allow_nan=False, default=str)
    sys.stdout.buffer.write(text.encode("utf-8", "backslashreplace"))
    sys.stdout.buffer.flush()
