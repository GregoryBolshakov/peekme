#!/usr/bin/env python3
"""A Pi chat with real prompts and answers, for tests that need Pi to draw a
conversation without asking a model: a saved session in an agent folder of
its own (PI_CODING_AGENT_DIR), opened with `pi --session <file>`, full screen.

Each turn is a prompt and an answer of labelled rows (`b<turn>r<row>`), as a
list, so every row stays a row. The folder's only model is the fake one of
pi_fake_model.py, which nothing here asks.
"""
import datetime
import json
import os
import uuid

import pi_fake_model


def pi_agent(root, turns, rows=40, cwd=None, long_prompt=(), repeated=(), wheel_rows=None):
    """An agent folder under `root` holding one saved chat; returns (folder,
    session file). `wheel_rows`: Pi's `fullscreenWheelScrollLines` (rows a
    wheel event), else its "auto"."""
    agent = pi_fake_model.pi_agent_dir(root, 18471)
    settings_path = os.path.join(agent, "settings.json")
    with open(settings_path) as f:
        settings = json.load(f)
    if wheel_rows:
        settings["fullscreenWheelScrollLines"] = wheel_rows
    with open(settings_path, "w") as f:
        json.dump(settings, f)
    cwd = cwd or os.getcwd()
    folder = "--" + cwd.lstrip("/\\").replace("/", "-").replace("\\", "-").replace(":", "-") + "--"
    sessions = os.path.join(agent, "sessions", folder)
    os.makedirs(sessions, exist_ok=True)
    sid = str(uuid.uuid4())
    now = datetime.datetime.now(datetime.timezone.utc)
    stamp = now.strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
    recs = [{"type": "session", "version": 3, "id": sid, "timestamp": stamp, "cwd": cwd},
            {"type": "model_change", "id": "m0", "parentId": None, "timestamp": stamp,
             "provider": "fake", "modelId": "fake-model"}]
    parent = "m0"
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
        ms = int(now.timestamp() * 1000) + 2000 * t
        for role, content, extra in (
                ("user", [{"type": "text", "text": prompt}], {}),
                ("assistant", [{"type": "text", "text": answer}],
                 {"api": "openai-completions", "provider": "fake", "model": "fake-model",
                  "stopReason": "stop",
                  "usage": {"input": 100, "output": 50, "cacheRead": 0, "cacheWrite": 0,
                            "totalTokens": 150,
                            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                                     "total": 0}}})):
            eid = uuid.uuid4().hex[:8]
            recs.append({"type": "message", "id": eid, "parentId": parent, "timestamp": stamp,
                         "message": {"role": role, "content": content, "timestamp": ms, **extra}})
            parent = eid
    path = os.path.join(sessions, f"{stamp.replace(':', '-').replace('.', '-')}_{sid}.jsonl")
    with open(path, "w") as f:
        for r in recs:
            f.write(json.dumps(r) + "\n")
    return agent, path
