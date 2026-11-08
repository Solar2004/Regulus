#!/usr/bin/env python3
"""A Regulus local module in plain Python (stdlib only).

The core runs this file and talks JSON-RPC 2.0 over stdin/stdout, one object per line
(docs/03-modules.md §6). Two node types: `echo` puts on `out` what arrives on `in`;
`shout` does the same in upper case. Logs go to stderr: the core keeps the last lines.
"""
import json
import pathlib
import sys

MANIFEST = pathlib.Path(__file__).with_name("regulus-module.toml")
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602

TRANSFORMS = {
    "echo": lambda text: text,
    "shout": lambda text: text.upper(),
}


def send(message):
    message["jsonrpc"] = "2.0"
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def emit(execution_id, node_id, port, value):
    """ModuleCtx::emit: a notification, no answer."""
    send({"method": "emit", "params": {
        "execution_id": execution_id, "node_id": node_id, "port": port, "value": value,
    }})


class Failure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def node_event(call):
    """`node/event`: a value arrived on a "then" input of one of our nodes."""
    node, event = call["node"], call["args"]
    transform = TRANSFORMS.get(node["type"])
    if transform is None or event["port"] != "in":
        raise Failure(INVALID_PARAMS, f"{node['type']} takes nothing on {event['port']}")
    if not isinstance(event["value"], str):
        raise Failure(INVALID_PARAMS, "text@1 must be a string")
    emit(call["execution_id"], node["id"], "out", transform(event["value"]))
    return None


def handle(method, params):
    if method == "manifest":
        return MANIFEST.read_text()  # the TOML text; a JSON object works too
    if method in ("start", "stop"):
        return None
    if method == "call":
        if params["method"] == "node/event":
            return node_event(params["params"])
        # node/start, node/stop and the rest: we have no lifecycle per node.
        raise Failure(METHOD_NOT_FOUND, f"unknown method `{params['method']}`")
    raise Failure(METHOD_NOT_FOUND, f"unknown method `{method}`")


def main():
    for line in sys.stdin:
        if not line.strip():
            continue
        request = json.loads(line)
        if "id" not in request:
            continue  # nothing is notified to us today
        try:
            result = handle(request["method"], request.get("params") or {})
            send({"id": request["id"], "result": result})
        except Failure as failure:
            send({"id": request["id"], "error": {"code": failure.code, "message": str(failure)}})
        except Exception as error:  # a bug answers as an error, never kills the module
            print(f"echo: {error!r}", file=sys.stderr, flush=True)
            send({"id": request["id"], "error": {"code": -32000, "message": repr(error)}})
        if request["method"] == "stop":
            break


if __name__ == "__main__":
    main()
