"""Tests of the Python SDK, speaking the real protocol to real processes.

Run: python3 -B -m unittest discover -s sdk/python/tests -v  (-B: no __pycache__ left behind)
"""

import json
import os
import pathlib
import queue
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import unittest

SDK = pathlib.Path(__file__).resolve().parents[1]
REPO = SDK.parents[1]
EXAMPLE = REPO / "examples" / "local-modules" / "python-sdk" / "run.py"
TIMEOUT = 10

# Tests leave no __pycache__ in the source tree: not this process, nor the modules it runs.
sys.dont_write_bytecode = True
sys.path.insert(0, str(SDK))
import regulus  # noqa: E402

# A module for the tests: what each node type does is driven by the value it receives.
MODULE = textwrap.dedent(
    """
    import os, subprocess, sys, time
    import regulus as rg

    @rg.node("probe", name="Probe",
             inputs=[rg.then_in("in", "json@1")],
             outputs=[rg.then_out("out", "json@1")],
             fields=[rg.field("prefix", "text", default="p:"),
                     rg.field("body", "code", language="python", multiline=True),
                     rg.field("items", "json", default=[])])
    def probe(ctx, port, value):
        action = value.get("do")
        if action == "echo":
            ctx.emit("out", ctx.config["prefix"] + value["text"])
        elif action == "sleep":
            time.sleep(value["seconds"])
            ctx.emit("out", value["tag"])
        elif action == "print":
            print("printed by the handler")
            subprocess.run(["sh", "-c", "echo from a child process"], check=True)
            ctx.emit("out", "after print")
        elif action == "read":
            ctx.emit("out", sys.stdin.read())
        elif action == "invalid":
            raise rg.InvalidInput("not acceptable")
        elif action == "fail":
            raise rg.NodeError("failed on purpose")
        elif action == "bug":
            raise KeyError("missing")
        elif action == "wrong-port":
            ctx.emit("nope", 1)
        elif action == "not-json":
            ctx.emit("out", {1, 2})
        elif action == "return":
            return "ignored"
        elif action == "exit":
            sys.exit(3)
        elif action == "interrupt":
            raise KeyboardInterrupt
        elif action == "surrogate":
            ctx.emit("out", os.fsdecode(b"bad \\xff"))
        elif action == "surrogate-error":
            raise rg.NodeError("bad " + os.fsdecode(b"\\xff") + " name")
        elif action == "big":
            ctx.emit("out", "x" * value["size"])
        elif action == "big-error":
            raise rg.NodeError("x" * value["size"])
        elif action == "append":
            ctx.config["items"].append(value["item"])
            ctx.emit("out", ctx.config["items"])
        elif action == "context":
            ctx.emit("out", {"execution": ctx.execution_id, "node": ctx.node_id,
                             "type": ctx.node_type, "project": ctx.project_id,
                             "root": ctx.root, "config": ctx.config,
                             "capability": ctx.event.get("capability")})

    rg.run(**ARGS)
    """
)


def strict_json(line):
    """``line`` parsed as the core parses it: serde_json refuses lone surrogates, which
    Python's ``json`` accepts as ``\\udcff`` escapes."""
    message = json.loads(line)
    try:
        json.dumps(message, ensure_ascii=False).encode("utf-8")
    except UnicodeEncodeError as error:
        raise AssertionError(f"the core would drop this line ({error}): {line[:200]}")
    return message


class Module:
    """A module process and the lines it writes, read on a thread.

    Blank lines are skipped, as the core does. Any other line that is not JSON fails the
    test, unless ``strict`` is off: then it is kept in ``noise``, as the core logs it.
    """

    def __init__(self, script, env=None, strict=True):
        env = dict(os.environ if env is None else env)
        env.setdefault("PYTHONDONTWRITEBYTECODE", "1")
        self.process = subprocess.Popen(
            [sys.executable, str(script)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        self.lines = queue.Queue()
        self.strict = strict
        self.noise = []
        self.next_id = 100
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for raw in self.process.stdout:
            self.lines.put(raw.decode("utf-8"))
        self.lines.put(None)

    def send(self, message):
        message["jsonrpc"] = "2.0"
        self.process.stdin.write((json.dumps(message) + "\n").encode())
        self.process.stdin.flush()

    def raw(self, text):
        self.process.stdin.write(text.encode())
        self.process.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        self.send({"id": self.next_id, "method": method, "params": params or {}})
        return self.next_id

    def read(self):
        while True:
            line = self.lines.get(timeout=TIMEOUT)
            if line is None:
                raise AssertionError("the module closed its stdout")
            if not line.strip():
                continue
            try:
                return strict_json(line)
            except ValueError:
                if self.strict:
                    raise
                self.noise.append(line.rstrip("\n"))

    def until_answer(self, request_id):
        """The emits before the answer to ``request_id``, and that answer."""
        emits = []
        while True:
            message = self.read()
            if message.get("id") == request_id:
                return emits, message
            emits.append(message)

    def event(self, value, node_type="probe", port="in", config=None, execution="exec-1"):
        return self.request("call", {"method": "node/event", "params": {
            "execution_id": execution,
            "node": {"id": "n1", "type": node_type, "config": config or {}},
            "args": {"port": port, "capability": "json@1", "value": value,
                     "from": {"node": "src", "port": "out"}, "event": 7},
        }})

    def finish(self):
        """Stops it and returns its exit code and stderr."""
        if self.process.poll() is None:
            answer = self.until_answer(self.request("stop"))[1]
            assert answer["result"] is None, answer
        self.process.stdin.close()
        code = self.process.wait(timeout=TIMEOUT)
        stderr = self.process.stderr.read().decode("utf-8")
        self.process.stderr.close()
        self.process.stdout.close()
        return code, stderr


class ProtocolTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        folder = pathlib.Path(self.dir.name)
        (folder / "regulus.py").write_text((SDK / "regulus.py").read_text())
        self.folder = folder

    def module(self, args="", env=None):
        script = self.folder / "run.py"
        script.write_text(MODULE.replace("**ARGS", args))
        full = {"PATH": os.environ.get("PATH", "")}
        full.update(env or {})
        module = Module(script, env=full)
        self.addCleanup(lambda: module.process.poll() is None and module.process.kill())
        return module

    def started(self, args="", env=None):
        module = self.module(args, env)
        _, manifest = module.until_answer(module.request("manifest", {"protocol": "1.16"}))
        self.assertIn("result", manifest)
        _, start = module.until_answer(module.request("start", {
            "protocol": "1.16", "module_id": "demo/probe@0.0.0",
            "project_id": "demo", "root": "/projects/demo"}))
        self.assertEqual(start["result"], None)
        return module

    def test_manifest_declares_nodes_ports_and_field_extras(self):
        module = self.module("name='Probe module', version='1.2.3'")
        _, answer = module.until_answer(module.request("manifest"))
        manifest = answer["result"]
        self.assertEqual(manifest["module"], {
            "requires_proto": "^1", "name": "Probe module", "id": "local/sdk@1.2.3"})
        (node,) = manifest["node"]
        self.assertEqual(node["type"], "probe")
        self.assertEqual([p["id"] for p in node["port"]], ["in", "out"])
        self.assertEqual(node["port"][0]["cardinality"], "many")
        self.assertEqual(node["port"][0]["kind"], "then")
        self.assertEqual(node["field"][0], {"id": "prefix", "type": "text", "default": "p:"})
        self.assertEqual(node["field"][1], {
            "id": "body", "type": "code", "language": "python", "multiline": True})
        self.assertEqual(module.finish()[0], 0)

    def test_event_emits_before_its_answer_with_defaults_merged(self):
        module = self.started()
        emits, answer = module.until_answer(module.event({"do": "echo", "text": "hi"}))
        self.assertEqual(answer, {"id": answer["id"], "result": None, "jsonrpc": "2.0"})
        self.assertEqual(emits, [{"jsonrpc": "2.0", "method": "emit", "params": {
            "execution_id": "exec-1", "node_id": "n1", "port": "out", "value": "p:hi"}}])
        emits, _ = module.until_answer(
            module.event({"do": "echo", "text": "hi"}, config={"prefix": ">"}))
        self.assertEqual(emits[0]["params"]["value"], ">hi")
        self.assertEqual(module.finish()[0], 0)

    def test_context_carries_node_execution_and_project(self):
        module = self.started()
        emits, _ = module.until_answer(module.event({"do": "context"}, execution="exec-9"))
        self.assertEqual(emits[0]["params"]["value"], {
            "execution": "exec-9", "node": "n1", "type": "probe", "project": "demo",
            "root": "/projects/demo", "config": {"prefix": "p:", "items": []},
            "capability": "json@1"})
        module.finish()

    def test_errors_answer_with_the_codes_the_core_maps_and_it_keeps_running(self):
        module = self.started()
        cases = [
            ({"do": "invalid"}, {}, -32602, "not acceptable"),
            ({"do": "fail"}, {}, -32000, "failed on purpose"),
            ({"do": "bug"}, {}, -32000, "KeyError"),
            ({"do": "wrong-port"}, {}, -32000, "has no output `nope`"),
            ({"do": "not-json"}, {}, -32000, "must be JSON"),
            ({"do": "echo"}, {"port": "other"}, -32602, "takes nothing on `other`"),
            ({"do": "echo"}, {"node_type": "ghost"}, -32602, "no node type `ghost`"),
        ]
        for value, where, code, text in cases:
            with self.subTest(value=value, where=where):
                emits, answer = module.until_answer(module.event(value, **where))
                self.assertEqual(emits, [])
                self.assertEqual(answer["error"]["code"], code)
                self.assertIn(text, answer["error"]["message"])
        emits, answer = module.until_answer(module.event({"do": "echo", "text": "still"}))
        self.assertEqual(emits[0]["params"]["value"], "p:still")
        code, stderr = module.finish()
        self.assertEqual(code, 0)
        self.assertIn("Traceback", stderr)  # the bug, for the core's log

    def test_node_lifecycle_and_unknown_methods_are_not_found(self):
        module = self.started()
        for method in ("node/start", "node/stop", "node/child", "node/hook"):
            _, answer = module.until_answer(module.request("call", {"method": method, "params": {}}))
            self.assertEqual(answer["error"]["code"], -32601, method)
        _, answer = module.until_answer(module.request("module.reload"))
        self.assertEqual(answer["error"]["code"], -32601)
        module.finish()

    def test_exit_or_interrupt_in_a_handler_answers_and_keeps_the_worker(self):
        # With one worker, a handler that ends its thread would leave every call unanswered.
        module = self.started("max_workers=1")
        for action, name in (("exit", "SystemExit: 3"), ("interrupt", "KeyboardInterrupt")):
            with self.subTest(action=action):
                emits, answer = module.until_answer(module.event({"do": action}))
                self.assertEqual(emits, [])
                self.assertEqual(answer["error"]["code"], -32000)
                self.assertIn(name, answer["error"]["message"])
        emits, _ = module.until_answer(module.event({"do": "echo", "text": "alive"}))
        self.assertEqual(emits[0]["params"]["value"], "p:alive")
        self.assertEqual(module.finish()[0], 0)

    def test_lifecycle_calls_are_answered_while_every_worker_is_busy(self):
        module = self.started("max_workers=1")
        slow = module.event({"do": "sleep", "seconds": 2, "tag": "slow"})
        time.sleep(0.2)  # the only worker is now in the slow handler
        began = time.monotonic()
        for method in ("node/start", "node/stop"):
            _, answer = module.until_answer(module.request("call", {"method": method, "params": {}}))
            self.assertEqual(answer["error"]["code"], -32601, method)
        self.assertLess(time.monotonic() - began, 1.0)
        emits, answer = module.until_answer(slow)
        self.assertEqual([e["params"]["value"] for e in emits], ["slow"])
        module.finish()

    def test_text_the_core_cannot_parse_never_leaves_the_process(self):
        # Lone surrogates (from os.fsdecode, surrogateescape…) make serde_json drop the line:
        # an emit would vanish and an error answer would leave the call hanging.
        module = self.started()
        emits, answer = module.until_answer(module.event({"do": "surrogate"}))
        self.assertEqual(emits, [])
        self.assertEqual(answer["error"]["code"], -32000)
        self.assertIn("must be JSON", answer["error"]["message"])
        emits, answer = module.until_answer(module.event({"do": "surrogate-error"}))
        self.assertEqual(answer["error"]["code"], -32000)
        self.assertEqual(answer["error"]["message"], "bad \\udcff name")
        emits, _ = module.until_answer(module.event({"do": "echo", "text": "ñ ✓ 😀"}))
        self.assertEqual(emits[0]["params"]["value"], "p:ñ ✓ 😀")
        module.finish()

    def test_a_large_value_round_trips_quickly(self):
        # 4 MiB in and out; read byte by byte it took ~5 s (review, 2026-11-10).
        module = self.started()
        text = "y" * (4 * 1024 * 1024)
        began = time.monotonic()
        emits, answer = module.until_answer(module.event({"do": "echo", "text": text}))
        elapsed = time.monotonic() - began
        self.assertEqual(answer["result"], None)
        self.assertEqual(len(emits[0]["params"]["value"]), len(text) + 2)
        self.assertLess(elapsed, 2.0)
        module.finish()

    def test_a_line_longer_than_the_core_takes_is_a_handler_error(self):
        # The core kills a process that writes a line past `max_line_bytes`.
        module = self.started(env={"REGULUS_MAX_LINE_BYTES": "4096"})
        emits, answer = module.until_answer(module.event({"do": "big", "size": 8192}))
        self.assertEqual(emits, [])
        self.assertEqual(answer["error"]["code"], -32000)
        self.assertIn("4096", answer["error"]["message"])
        _, answer = module.until_answer(module.event({"do": "big-error", "size": 8192}))
        self.assertEqual(answer["error"]["code"], -32000)
        self.assertIn("4096", answer["error"]["message"])
        emits, _ = module.until_answer(module.event({"do": "big", "size": 100}))
        self.assertEqual(len(emits[0]["params"]["value"]), 100)
        module.finish()
        module = self.module(env={"REGULUS_MAX_LINE_BYTES": "lots"})
        self.assertNotEqual(module.process.wait(timeout=TIMEOUT), 0)
        self.assertIn("REGULUS_MAX_LINE_BYTES", module.process.stderr.read().decode())
        for stream in (module.process.stdin, module.process.stdout, module.process.stderr):
            stream.close()

    def test_field_defaults_are_fresh_for_every_event(self):
        module = self.started()
        for item in ("a", "b"):
            emits, _ = module.until_answer(module.event({"do": "append", "item": item}))
        self.assertEqual(emits[0]["params"]["value"], ["b"])
        module.finish()

    def test_a_partial_line_printed_before_run_does_not_break_the_manifest(self):
        script = self.folder / "early.py"
        script.write_text(textwrap.dedent("""
            import regulus as rg
            print("loading...", end="")

            @rg.node("x", outputs=[rg.then_out("o", "text@1")])
            def x(ctx, port, value):
                pass

            rg.run()
            """))
        module = Module(script, env={"PATH": os.environ.get("PATH", "")}, strict=False)
        self.addCleanup(lambda: module.process.poll() is None and module.process.kill())
        _, answer = module.until_answer(module.request("manifest"))
        self.assertEqual(answer["result"]["node"][0]["type"], "x")
        self.assertEqual(module.noise, ["loading..."])
        self.assertEqual(module.finish()[0], 0)

    def test_noise_on_stdin_is_skipped(self):
        module = self.started()
        module.raw("not json\n\n[1,2]\n")
        module.send({"method": "notify", "params": {}})
        module.send({"id": 5, "result": None})
        emits, _ = module.until_answer(module.event({"do": "echo", "text": "ok"}))
        self.assertEqual(emits[0]["params"]["value"], "p:ok")
        _, stderr = module.finish()
        self.assertIn("not JSON on stdin", stderr)

    def test_prints_and_child_stdout_go_to_stderr_and_stdin_is_private(self):
        module = self.started()
        emits, answer = module.until_answer(module.event({"do": "print"}))
        self.assertEqual([e["params"]["value"] for e in emits], ["after print"])
        emits, answer = module.until_answer(module.event({"do": "read"}))
        self.assertEqual(emits[0]["params"]["value"], "")  # /dev/null, not the protocol
        emits, answer = module.until_answer(module.event({"do": "return"}))
        self.assertEqual(answer["result"], None)
        code, stderr = module.finish()
        self.assertEqual(code, 0)
        self.assertIn("printed by the handler", stderr)
        self.assertIn("from a child process", stderr)
        self.assertIn("returned a value; use ctx.emit", stderr)

    def test_a_slow_event_does_not_hold_another(self):
        module = self.started()
        slow = module.event({"do": "sleep", "seconds": 1.5, "tag": "slow"}, execution="a")
        fast = module.event({"do": "sleep", "seconds": 0, "tag": "fast"}, execution="b")
        order = [module.read() for _ in range(4)]
        ids = [m.get("id") for m in order if "id" in m]
        self.assertEqual(ids, [fast, slow])
        # Each emit comes before the answer of its own call.
        tags = [m["params"]["value"] if "method" in m else m["id"] for m in order]
        self.assertEqual(tags, ["fast", fast, "slow", slow])
        module.finish()

    def test_max_workers_bounds_concurrency(self):
        for config, env in (("max_workers=1", None), ("", {"REGULUS_MAX_WORKERS": "1"})):
            with self.subTest(config=config, env=env):
                module = self.started(config, env)
                slow = module.event({"do": "sleep", "seconds": 0.5, "tag": "slow"})
                fast = module.event({"do": "sleep", "seconds": 0, "tag": "fast"})
                answers = []
                while len(answers) < 2:
                    message = module.read()
                    if "id" in message:
                        answers.append(message["id"])
                self.assertEqual(answers, [slow, fast])
                module.finish()

    def test_many_concurrent_events_lose_nothing(self):
        module = self.started("max_workers=8")
        sent = {module.event({"do": "echo", "text": str(n)}, execution=f"e{n}"): n
                for n in range(200)}
        answered, values = set(), set()
        while len(answered) < len(sent):
            message = module.read()  # each line parses: no interleaved writes
            if "id" in message:
                self.assertIsNone(message["result"])
                answered.add(message["id"])
            else:
                values.add(message["params"]["value"])
        self.assertEqual(answered, set(sent))
        self.assertEqual(values, {f"p:{n}" for n in range(200)})
        module.finish()

    def test_stdin_closing_ends_it(self):
        module = self.started()
        module.process.stdin.close()
        self.assertEqual(module.process.wait(timeout=TIMEOUT), 0)
        module.process.stderr.close()
        module.process.stdout.close()

    def test_bad_max_workers_and_no_nodes_fail_at_start(self):
        module = self.module("max_workers=0")
        code = module.process.wait(timeout=TIMEOUT)
        stderr = module.process.stderr.read().decode()
        module.process.stdin.close()
        module.process.stdout.close()
        module.process.stderr.close()
        self.assertNotEqual(code, 0)
        self.assertIn("max_workers must be at least 1", stderr)

        script = self.folder / "empty.py"
        script.write_text("import regulus\nregulus.run()\n")
        done = subprocess.run([sys.executable, str(script)], capture_output=True,
                              text=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("no node types", done.stderr)
        self.assertEqual(done.stdout, "")


class DeclarationTest(unittest.TestCase):
    def tearDown(self):
        regulus._NODES.clear()

    def test_declaration_errors_are_caught_at_import(self):
        cases = [
            lambda: regulus.then_in("", "text@1"),
            lambda: regulus.then_in("in", ""),
            lambda: regulus.then_in("in", "text@1", cardinality="few"),
            lambda: regulus.field("x", default={1}),
            lambda: regulus.field("x", default=float("nan")),
            lambda: regulus.field("x", options=[object()]),
            lambda: regulus.field("x", type="code"),
            lambda: regulus.node("a", inputs=[regulus.then_out("o", "text@1")]),
            lambda: regulus.node("a", outputs=[regulus.then_in("i", "text@1")]),
            lambda: regulus.node("a", inputs=[regulus.then_in("p", "text@1")],
                                 outputs=[regulus.then_out("p", "text@1")]),
            lambda: regulus.node("a", fields=[regulus.field("f"), regulus.field("f")]),
            lambda: regulus.node("")(lambda *a: None),
        ]
        for index, case in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises((ValueError, TypeError)):
                    case()

    def test_a_node_type_is_declared_once(self):
        regulus.node("a")(lambda ctx, port, value: None)
        with self.assertRaises(ValueError):
            regulus.node("a")(lambda ctx, port, value: None)

    def test_manifest_without_name_or_version_leaves_them_to_the_core(self):
        regulus.node("a", outputs=[regulus.then_out("o", "text@1")])(lambda *a: None)
        manifest = regulus.manifest()
        self.assertEqual(manifest["module"], {"requires_proto": "^1"})
        self.assertEqual(manifest["node"][0]["port"][0], {
            "id": "o", "name": "", "capability": "text@1", "direction": "output", "kind": "then"})


class ExampleTest(unittest.TestCase):
    def test_the_example_runs_in_place(self):
        module = Module(EXAMPLE, env={"PATH": os.environ.get("PATH", "")})
        _, answer = module.until_answer(module.request("manifest"))
        types = [n["type"] for n in answer["result"]["node"]]
        self.assertEqual(types, ["template", "lines"])
        pattern = answer["result"]["node"][0]["field"][0]
        self.assertEqual((pattern["type"], pattern["language"]), ("code", "python"))
        module.until_answer(module.request("start", {"project_id": "demo", "root": "/"}))

        emits, _ = module.until_answer(module.event("hi", node_type="template",
                                                    config={"pattern": "<{value}>"}))
        self.assertEqual([e["params"]["value"] for e in emits], ["<hi>"])
        emits, _ = module.until_answer(module.event(" a \n\nb", node_type="lines"))
        self.assertEqual([(e["params"]["port"], e["params"]["value"]) for e in emits],
                         [("out", "a"), ("out", "b"), ("count", 2)])
        _, answer = module.until_answer(module.event("x", node_type="template",
                                                     config={"pattern": "{nope}"}))
        self.assertEqual(answer["error"]["code"], -32602)
        _, answer = module.until_answer(module.event("x", node_type="template",
                                                     config={"pattern": "{value.x}"}))
        self.assertEqual(answer["error"]["code"], -32602)
        _, answer = module.until_answer(module.event(3, node_type="lines"))
        self.assertEqual(answer["error"]["code"], -32602)
        self.assertEqual(module.finish()[0], 0)


if __name__ == "__main__":
    unittest.main()
