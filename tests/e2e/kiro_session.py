#!/usr/bin/env python3
"""A Kiro CLI chat with real prompts and answers, for tests that need Kiro to
draw a conversation without asking a model: a saved session in a KIRO_HOME of
its own, opened with `kiro-cli chat --resume-id`, starting full screen.

Each turn is a prompt and an answer of labelled rows (`b<turn>r<row>`), as a
list, so every row stays a row. Kiro's sign-in stays where it is
(~/.local/share/kiro-cli), outside KIRO_HOME: nothing of it is copied.
"""
import datetime
import json
import os
import time
import uuid


def kiro_home(root, turns, rows=40, cwd=None, long_prompt=(), repeated=(), wheel_rows=None,
              fullscreen=True):
    """A KIRO_HOME under `root` holding one saved chat; returns (folder,
    session id). Settings: start full screen (inline with `fullscreen`
    false), and `wheel_rows` rows a turn of the wheel when given (Kiro's
    default is 2)."""
    home = os.path.join(root, "kiro-home")
    sessions = os.path.join(home, "sessions", "cli")
    os.makedirs(sessions, exist_ok=True)
    os.makedirs(os.path.join(home, "settings"), exist_ok=True)
    settings = {"chat.startFullscreen": True} if fullscreen else {}
    if wheel_rows:
        settings["chat.fullscreenWheelRows"] = wheel_rows
    with open(os.path.join(home, "settings", "cli.json"), "w") as f:
        json.dump(settings, f)
    cwd = cwd or os.getcwd()
    sid = str(uuid.uuid4())
    now = datetime.datetime.now(datetime.timezone.utc)
    stamp = now.strftime("%Y-%m-%dT%H:%M:%S.000000000Z")
    recs = []
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
        recs.append({"version": "v1", "kind": "Prompt",
                     "data": {"message_id": str(uuid.uuid4()),
                              "content": [{"kind": "text", "data": prompt}],
                              "meta": {"timestamp": int(time.time()) + 2 * t}}})
        recs.append({"version": "v1", "kind": "AssistantMessage",
                     "data": {"message_id": str(uuid.uuid4()),
                              "content": [{"kind": "text", "data": answer}]}})
    with open(os.path.join(sessions, f"{sid}.jsonl"), "w") as f:
        for r in recs:
            f.write(json.dumps(r) + "\n")
    with open(os.path.join(sessions, f"{sid}.json"), "w") as f:
        json.dump({"session_id": sid, "cwd": cwd, "created_at": stamp, "updated_at": stamp,
                   "title": "Scroll test", "session_state": {"version": "v1"}}, f)
    return home, sid
