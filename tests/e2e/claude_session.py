#!/usr/bin/env python3
"""A Claude Code chat with real prompts and answers, for tests that need
Claude to draw a conversation without asking a model: a saved session in a
config folder of its own, opened with `claude --resume`.

Each turn is a prompt and an answer of labelled rows (`b<turn>r<row>`), as a
list, so every row stays a row.
"""
import datetime
import json
import os
import shutil
import subprocess
import uuid


def source_dir():
    return os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")


def config_dir(root, turns, rows=40, cwd=None, long_prompt=(), repeated=()):
    """A Claude config folder under `root` holding one saved chat; returns
    (folder, session id). Settings and sign-in come from the real folder,
    nothing is written there; call `forget_sign_in` when done."""
    home = os.path.join(root, "claude-config")
    os.makedirs(home, exist_ok=True)
    src = source_dir()
    # The main config file sits in the folder when CLAUDE_CONFIG_DIR is set,
    # else in the home folder next to ~/.claude.
    main = (os.path.join(src, ".claude.json") if os.environ.get("CLAUDE_CONFIG_DIR")
            else os.path.expanduser("~/.claude.json"))
    for path in (os.path.join(src, ".credentials.json"), main, os.path.join(src, "settings.json")):
        if os.path.exists(path):
            shutil.copy(path, os.path.join(home, os.path.basename(path)))
    cwd = cwd or os.getcwd()
    sid = str(uuid.uuid4())
    project = os.path.join(home, "projects", cwd.replace("/", "-").replace(".", "-"))
    os.makedirs(project, exist_ok=True)
    now = datetime.datetime.now(datetime.timezone.utc)
    common = {"isSidechain": False, "userType": "external", "entrypoint": "cli", "cwd": cwd,
              "sessionId": sid, "version": "2.1.299", "gitBranch": "main"}
    recs, parent = [], None
    for t in range(turns):
        prompt = f"q{t} show block {t}"
        if t in long_prompt:
            prompt += " " + " ".join(f"and some more words of the prompt {i}" for i in range(12))
        if t in repeated:
            # Rows that all read the same, between two labelled ones.
            answer = "\n".join([f"- b{t}r00 start"] + ["- same row of a repeated output"] * 30
                               + [f"- b{t}r99 end"])
        else:
            answer = "\n".join(f"- b{t}r{i:02d} words of row {i} in block {t}"
                               for i in range(1, rows + 1))
        stamp = (now + datetime.timedelta(seconds=2 * t)).strftime("%Y-%m-%dT%H:%M:%S.000Z")
        u, a = str(uuid.uuid4()), str(uuid.uuid4())
        recs.append({"parentUuid": parent, "type": "user", "uuid": u, "timestamp": stamp,
                     "promptId": str(uuid.uuid4()),
                     "message": {"role": "user", "content": prompt}, **common})
        recs.append({"parentUuid": u, "type": "assistant", "uuid": a, "timestamp": stamp,
                     "message": {"model": "claude-opus-5-5", "id": f"msg_{uuid.uuid4().hex}",
                                 "type": "message", "role": "assistant",
                                 "content": [{"type": "text", "text": answer}],
                                 "stop_reason": "end_turn", "stop_sequence": None,
                                 "usage": {"input_tokens": 1, "output_tokens": 1}},
                     "requestId": f"req_{uuid.uuid4().hex}", **common})
        parent = a
    with open(os.path.join(project, f"{sid}.jsonl"), "w") as f:
        for r in recs:
            f.write(json.dumps(r) + "\n")
    return home, sid


def forget_sign_in(home):
    """Remove the copied sign-in and config from a folder made by `config_dir`."""
    for name in (".credentials.json", ".claude.json"):
        path = os.path.join(home, name)
        if os.path.exists(path):
            subprocess.run(["shred", "-u", path], check=False)
            if os.path.exists(path):
                os.remove(path)
