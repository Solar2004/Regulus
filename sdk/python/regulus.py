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

It is not a sandbox: the process runs with the user's permissions, only in a project the
user trusts. Copy this file beside your ``run.py`` (``regulus node init`` does it).
"""

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
    "node",
    "run",
    "then_in",
    "then_out",
]

#: The core protocol major this SDK speaks (docs/03-modules.md §6).
REQUIRES_PROTO = "^1"
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


def _port(direction, port_id, capability, name, required, cardinality, lazy):
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
    return port


def then_in(port_id, capability, *, name="", required=False, cardinality="many", lazy=False):
    """A "then" input: values arrive on it as ``handler(ctx, port_id, value)``.

    ``cardinality`` is ``"many"`` by default, so several wires can feed it.
    """
    return _port(INPUT, port_id, capability, name, required, cardinality, lazy)


def then_out(port_id, capability, *, name=""):
    """A "then" output: ``ctx.emit(port_id, value)`` puts a value on it."""
    return _port(OUTPUT, port_id, capability, name, False, None, False)


def field(field_id, field_type="text", *, default=None, options=None, **extra):
    """An option of the node, drawn by the inspector and read as ``ctx.config[field_id]``.

    ``field_type`` is open (``text``, ``number``, ``select``, ``code``…): a client that
    does not know it shows the raw value. ``extra`` keys travel with the field as they are
    (``label``, ``description``, ``multiline``, ``language``…) for clients that use them.
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
    for key, value in extra.items():
        spec[key] = _json_value(f"field `{field_id}` `{key}`", value)
    return spec


class _NodeType:
    def __init__(self, node_type, name, ports, fields, handler):
        self.node_type = node_type
        self.name = name
        self.ports = ports
        self.fields = fields
        self.handler = handler
        self.inputs = {p["id"] for p in ports if p["direction"] == INPUT}
        self.outputs = {p["id"] for p in ports if p["direction"] == OUTPUT}
        self.defaults = {f["id"]: f["default"] for f in fields if "default" in f}
        self.warned_return = False

    def manifest(self):
        return {"type": self.node_type, "name": self.name, "port": self.ports, "field": self.fields}


# The node types this process declares, in declaration order.
_NODES = {}


def _unique(kind, owner, ids):
    seen = set()
    for item in ids:
        if item in seen:
            raise ValueError(f"{owner}: {kind} `{item}` is declared twice")
        seen.add(item)


def node(node_type, *, name=None, inputs=(), outputs=(), fields=()):
    """Declares a node type whose "then" inputs are handled by the decorated function.

    The handler is called as ``handler(ctx, port, value)`` for each value that arrives;
    it puts values on its outputs with ``ctx.emit``. What it returns is ignored.
    """
    if not isinstance(node_type, str) or not node_type:
        raise ValueError("a node type needs a non-empty string")
    if node_type in _NODES:
        raise ValueError(f"node type `{node_type}` is declared twice")
    inputs, outputs, fields = list(inputs), list(outputs), list(fields)
    owner = f"node `{node_type}`"
    for port in inputs:
        if port.get("direction") != INPUT:
            raise ValueError(f"{owner}: `{port.get('id')}` in inputs is not then_in(...)")
    for port in outputs:
        if port.get("direction") != OUTPUT:
            raise ValueError(f"{owner}: `{port.get('id')}` in outputs is not then_out(...)")
    _unique("port", owner, [p["id"] for p in inputs + outputs])
    _unique("field", owner, [f["id"] for f in fields])

    def register(handler):
        if not callable(handler):
            raise TypeError(f"{owner}: the handler must be callable")
        _NODES[node_type] = _NodeType(node_type, name or node_type, inputs + outputs, fields, handler)
        return handler

    return register


def manifest(module_name=None, version=None):
    """The manifest the core asks for: the shape of ``regulus-module.toml`` as JSON.

    The core names the module ``<project>/<id>@<version>`` whatever is said here;
    ``version`` is used only when ``regulus.toml`` does not set one.
    """
    module: dict = {"requires_proto": REQUIRES_PROTO}
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

    def emit(self, port, value=None):
        """Puts ``value`` on the "then" output ``port`` of this node.

        Raises ``ValueError`` if the node type does not declare that output, the value
        cannot travel as JSON (or as UTF-8), or the line would be longer than the core
        reads; nothing is sent then.
        """
        if port not in self._type.outputs:
            declared = ", ".join(sorted(self._type.outputs)) or "none"
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
        if port not in node_type.inputs:
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
