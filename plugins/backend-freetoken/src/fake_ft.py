#!/usr/bin/env python3
"""Offline ft fixture: no FreeToken install, models, downloads, or GPU."""
import http.server
import json
import os
import sys
import time

print("fake ft startup noise must not enter JSON-RPC", flush=True)
print("fake ft diagnostic", file=sys.stderr, flush=True)

child = os.fork()
if child == 0:
    while True:
        time.sleep(60)

report_path = os.environ["FT_TEST_REPORT"]

class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != "/v1/chat/completions" or not request.get("model"):
            self.send_error(422)
            return
        chunks = [
            {"choices": [{"delta": {"content": "Grüße"}}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}]},
        ]
        body = ("".join("data: " + json.dumps(c) + "\n\n" for c in chunks)
                + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path != "/v1/cache/status":
            self.send_error(404)
            return
        body = json.dumps({"state": os.environ["FT_TEST_STATE"]}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()
        with open(report_path + ".responses", "a", encoding="utf-8") as responses:
            responses.write(os.environ["FT_TEST_STATE"] + "\n")

    def log_message(self, format, *args):
        pass

time.sleep(float(os.environ.get("FT_TEST_STARTUP_DELAY", "0")))
port = int(sys.argv[sys.argv.index("--port") + 1])
server = http.server.HTTPServer(("127.0.0.1", port), Handler)
# Publish only a complete report, after the listening socket exists.
with open(report_path + ".tmp", "w", encoding="utf-8") as report:
    json.dump({"pid": os.getpid(), "child": child, "args": sys.argv[1:],
               "value": os.environ["FT_TEST_VALUE"],
               "venv": os.environ["VIRTUAL_ENV"],
               "port": server.server_port}, report)
os.replace(report_path + ".tmp", report_path)
server.serve_forever()
