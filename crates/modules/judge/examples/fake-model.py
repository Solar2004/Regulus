#!/usr/bin/env python3
"""A stand-in for an OpenAI-compatible model server, for examples/judge.toml.

Serves POST /v1/chat/completions and answers the judge's schema: `pass` when the input
mentions a test, `fail` otherwise. Usage: fake-model.py [port] (default 11435).
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        prompt = body["messages"][-1]["content"]
        judged = prompt.rsplit("Input:\n", 1)[-1]
        passed = "test" in judged
        answer = {
            "verdict": "pass" if passed else "fail",
            "feedback": "has a test" if passed else "add a test that exercises it",
        }
        reply = {"choices": [{"message": {"role": "assistant", "content": json.dumps(answer)}}]}
        data = json.dumps(reply).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


port = int(sys.argv[1]) if len(sys.argv) > 1 else 11435
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
