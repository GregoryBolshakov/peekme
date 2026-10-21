#!/usr/bin/env python3
"""A local OpenAI-compatible chat endpoint for Pi, so tests see Pi draw and
save real answers without a login or a model call.

    python3 tests/e2e/pi_fake_model.py PORT [--delay SECS]

`pi_agent_dir(root, port)` writes a Pi agent folder (PI_CODING_AGENT_DIR)
whose only model is `fake/fake-model` on that port.

The answer depends on the last user message:
  - contains "tool": first a `bash` tool call (`ls`), then text after its result;
  - contains "rows N": N labelled rows (`r0 ...`), one per line;
  - otherwise a short markdown answer (heading, bold, list, code block).
Every answer streams a few words per chunk, `--delay` seconds apart.
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MARKDOWN = """## Where the config lives

The **settings file** is read once at start, then merged with the project's own:

- `~/.config/demo/settings.json` holds the user's defaults
- `.demo/settings.json` in the project overrides them
- environment variables win over both

```rust
fn load() -> Settings {
    let user = read(home().join("settings.json"));
    user.merge(read(".demo/settings.json"))
}
```

So a change in the project file applies to that project only."""


def answer_for(messages):
    last_user = next((m for m in reversed(messages) if m.get("role") == "user"), {})
    text = last_user.get("content", "")
    if isinstance(text, list):
        text = " ".join(p.get("text", "") for p in text if isinstance(p, dict))
    after_tool = messages and messages[-1].get("role") == "tool"
    if "tool" in text and not after_tool:
        return None  # a tool call
    if after_tool:
        return "The folder holds the files listed above; **main.rs** is the entry point."
    words = text.split()
    if "rows" in words:
        i = words.index("rows")
        n = int(words[i + 1]) if i + 1 < len(words) and words[i + 1].isdigit() else 40
        return "\n".join(f"r{k} row number {k} of the answer" for k in range(n))
    return MARKDOWN


class Handler(BaseHTTPRequestHandler):
    delay = 0.02

    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            body = json.dumps({"object": "list", "data": [{"id": "fake-model", "object": "model"}]})
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body.encode())
        else:
            self.send_error(404)

    def chunk(self, delta, finish=None):
        obj = {
            "id": "chatcmpl-fake",
            "object": "chat.completion.chunk",
            "created": int(time.time()),
            "model": "fake-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        }
        self.wfile.write(f"data: {json.dumps(obj)}\n\n".encode())
        self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        text = answer_for(req.get("messages", []))
        self.chunk({"role": "assistant", "content": ""})
        if text is None:
            call = {"index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "bash", "arguments": json.dumps({"command": "ls"})}}
            self.chunk({"tool_calls": [call]})
            self.chunk({}, "tool_calls")
        else:
            parts = text.split(" ")
            for i in range(0, len(parts), 3):
                piece = " ".join(parts[i:i + 3]) + (" " if i + 3 < len(parts) else "")
                self.chunk({"content": piece})
                time.sleep(self.delay)
            self.chunk({}, "stop")
        usage = {"id": "chatcmpl-fake", "object": "chat.completion.chunk", "created": 0,
                 "model": "fake-model", "choices": [],
                 "usage": {"prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150}}
        self.wfile.write(f"data: {json.dumps(usage)}\n\ndata: [DONE]\n\n".encode())
        self.wfile.flush()


def pi_agent_dir(root, port):
    """A Pi agent folder under `root` with the fake model as its default and
    no other provider; returns its path."""
    d = os.path.join(root, "pi-agent")
    os.makedirs(d, exist_ok=True)
    models = {"providers": {"fake": {
        "baseUrl": f"http://127.0.0.1:{port}/v1",
        "api": "openai-completions",
        "apiKey": "fake",
        "models": [{"id": "fake-model", "name": "Fake model", "contextWindow": 128000,
                    "maxTokens": 4096}],
    }}}
    with open(os.path.join(d, "models.json"), "w") as f:
        json.dump(models, f)
    settings = {"defaultProvider": "fake", "defaultModel": "fake-model",
                "defaultThinkingLevel": "off", "quietStartup": True,
                "defaultProjectTrust": "always", "enableInstallTelemetry": False}
    with open(os.path.join(d, "settings.json"), "w") as f:
        json.dump(settings, f)
    return d


def serve(port, delay=0.02):
    Handler.delay = delay
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    args = sys.argv[1:]
    delay = 0.02
    if "--delay" in args:
        i = args.index("--delay")
        delay = float(args[i + 1])
        del args[i:i + 2]
    serve(int(args[0]), delay)
