#!/usr/bin/env python3
"""Talk to `kiro-cli acp` the way peekme's explainer would, print every message and timings.

    python3 spikes/kiro_probe.py [MODEL] [PROMPT]

Starts the server, initialize -> session/new -> (model) -> session/prompt, then closes the
session and deletes it with `kiro-cli chat --delete-session`.
KPROBE_CWD runs the server elsewhere, KPROBE_ARGS adds `acp` arguments (`--agent NAME`).
"""
import json, os, subprocess, sys, time

model = sys.argv[1] if len(sys.argv) > 1 else ''
prompt = sys.argv[2] if len(sys.argv) > 2 else 'Explain in one sentence what `ls -la` prints.'
t0 = time.time()
# KPROBE_CWD: directory to run the server in; KPROBE_ARGS: extra `acp` arguments.
cwd = os.environ.get('KPROBE_CWD', os.getcwd())
p = subprocess.Popen(['kiro-cli', 'acp'] + os.environ.get('KPROBE_ARGS', '').split(),
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=cwd)
nid = 0


def ts():
    return f'{time.time() - t0:6.2f}'


def send(method, params, rid=True):
    global nid
    msg = {'jsonrpc': '2.0', 'method': method, 'params': params}
    if rid:
        nid += 1
        msg['id'] = nid
    p.stdin.write((json.dumps(msg) + '\n').encode())
    p.stdin.flush()
    return nid if rid else None


def wait(rid, show=True):
    while True:
        line = p.stdout.readline()
        if not line:
            print(ts(), 'EOF')
            sys.exit(1)
        m = json.loads(line)
        if show:
            s = json.dumps(m)
            print(ts(), s[:400] + ('...' if len(s) > 400 else ''))
        if 'method' in m and 'id' in m:
            # a request from the agent (permission, fs): refuse
            reply = {'jsonrpc': '2.0', 'id': m['id']}
            if m['method'] == 'session/request_permission':
                reply['result'] = {'outcome': {'outcome': 'cancelled'}}
            else:
                reply['error'] = {'code': -32601, 'message': 'not supported'}
            p.stdin.write((json.dumps(reply) + '\n').encode())
            p.stdin.flush()
        if m.get('id') == rid and 'method' not in m:
            return m


r = wait(send('initialize', {'protocolVersion': 1, 'clientCapabilities': {
    'fs': {'readTextFile': False, 'writeTextFile': False}, 'terminal': False},
    'clientInfo': {'name': 'peekme', 'version': '0'}}))
r = wait(send('session/new', {'cwd': cwd, 'mcpServers': []}))
sid = r['result']['sessionId']
if model:
    wait(send('session/set_model', {'sessionId': sid, 'modelId': model}))
t1 = time.time()
r = wait(send('session/prompt', {'sessionId': sid, 'prompt': [{'type': 'text', 'text': prompt}]}))
print(ts(), f'prompt took {time.time() - t1:.2f}s')
# Close the session first: an open one is written again after the delete.
wait(send('_kiro.dev/session/terminate', {'sessionId': sid}), show=False)
p.stdin.close()
p.terminate()
p.wait()
d = subprocess.run(['kiro-cli', 'chat', '--delete-session', sid], capture_output=True, text=True, cwd=cwd)
print(ts(), 'delete:', d.returncode, d.stdout.strip()[:200], d.stderr.strip()[:200])
