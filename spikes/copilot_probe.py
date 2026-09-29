#!/usr/bin/env python3
"""Talk to `copilot --headless --stdio` (the Copilot SDK's JSON-RPC server) and print timings.

    python3 spikes/copilot_probe.py info                 # ping, auth, models (no model call)
    python3 spikes/copilot_probe.py explain MODEL TEXT   # one session: create, send, stream, delete

Messages are JSON-RPC 2.0 with `Content-Length` framing (vscode-jsonrpc).
"""
import json, os, subprocess, sys, threading, time, queue

class Server:
    def __init__(self):
        self.t0 = time.time()
        self.p = subprocess.Popen(["copilot", "--headless", "--stdio", "--no-auto-update", "--log-level", "none"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  cwd=os.environ.get("PROBE_CWD", "/tmp"))
        self.next = 1
        self.replies = {}
        self.events = queue.Queue()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        f = self.p.stdout
        while True:
            headers = {}
            while True:
                line = f.readline()
                if not line:
                    self.events.put(None); return
                line = line.strip()
                if not line:
                    break
                k, _, v = line.decode().partition(":")
                headers[k.lower()] = v.strip()
            body = json.loads(f.read(int(headers["content-length"])))
            if "id" in body and "method" not in body:
                self.replies[body["id"]].put(body)
            elif "method" in body and "id" in body:
                # A request from the server (permission, user input): refuse it.
                self.send({"jsonrpc": "2.0", "id": body["id"], "error": {"code": -32601, "message": "not supported"}})
                self.events.put(body)
            else:
                self.events.put(body)

    def send(self, msg):
        data = json.dumps(msg).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
        self.p.stdin.flush()

    def call(self, method, params, timeout=60):
        i = self.next; self.next += 1
        self.replies[i] = queue.Queue()
        self.send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        r = self.replies[i].get(timeout=timeout)
        if "error" in r:
            raise RuntimeError(f"{method}: {r['error']}")
        return r.get("result")

    def ms(self):
        return f"{(time.time() - self.t0) * 1000:.0f} ms"

def main():
    s = Server()
    print("ping:", s.call("ping", {"message": "hi"}), "at", s.ms())
    print("auth:", {k: v for k, v in (s.call("auth.getStatus", {}) or {}).items() if k != "login"} , "at", s.ms())
    if sys.argv[1] == "info":
        models = s.call("models.list", {})["models"]
        for m in models:
            billing = m.get("billing") or {}
            print(f"  {m.get('id'):28} mult={billing.get('multiplier')!s:6} {m.get('name','')}")
        print("models at", s.ms())
    elif sys.argv[1] == "explain":
        import uuid
        model = sys.argv[2] if sys.argv[2] != "default" else None
        sid = str(uuid.uuid4())
        params = {"sessionId": sid, "clientName": "peekme", "availableTools": [], "streaming": True,
                  "systemMessage": {"mode": "replace", "content": "You explain a selected fragment of terminal text in 2-4 short sentences."}}
        if model:
            params["model"] = model
        if len(sys.argv) > 4:
            params["reasoningEffort"] = sys.argv[4]
        t = time.time()
        r = s.call("session.create", params)
        print("create:", {k: v for k, v in (r or {}).items() if k in ("sessionId", "model", "workspacePath")}, f"{(time.time()-t)*1000:.0f} ms")
        t = time.time()
        s.call("session.send", {"sessionId": sid, "prompt": sys.argv[3]})
        first = None; text = []; types = {}
        while True:
            ev = s.events.get(timeout=90)
            if ev is None:
                break
            e = (ev.get("params") or {}).get("event") or {}
            ty = e.get("type", ev.get("method"))
            types[ty] = types.get(ty, 0) + 1
            data = e.get("data") or {}
            if ty == "assistant.message_delta":
                if first is None:
                    first = time.time() - t
                text.append(data.get("deltaContent") or data.get("content") or "")
            if ty in ("assistant.usage",):
                print("usage:", {k: data.get(k) for k in ("model", "inputTokens", "outputTokens", "cost", "premiumRequests", "multiplier") if k in data})
            if ty in ("session.idle", "assistant.turn_end", "session.error"):
                if ty == "session.error":
                    print("ERROR:", json.dumps(data)[:400])
                if ty != "assistant.turn_end":
                    break
        print(f"first delta {first and first*1000:.0f} ms, total {(time.time()-t)*1000:.0f} ms" if first else f"no deltas, total {(time.time()-t)*1000:.0f} ms")
        print("events:", types)
        print("text:", "".join(text)[:400])
        print("delete:", s.call("session.delete", {"sessionId": sid}))
    s.p.terminate()

if __name__ == "__main__":
    main()
