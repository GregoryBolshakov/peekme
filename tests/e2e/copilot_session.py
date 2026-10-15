#!/usr/bin/env python3
"""A GitHub Copilot CLI chat with real prompts and answers, for tests that
need Copilot to draw a conversation without asking a model: a saved session
in a COPILOT_HOME of its own, opened with `copilot --resume`.

Each turn is a prompt and an answer of labelled rows (`b<turn>r<row>`), as a
list, so every row stays a row.
"""
import datetime
import json
import os
import re
import uuid


def source_dir():
    return os.environ.get("COPILOT_HOME") or os.path.expanduser("~/.copilot")


def read_config(path):
    """Copilot's config.json starts with `//` comment lines."""
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except FileNotFoundError:
        return {}
    return json.loads(re.sub(r"(?m)^\s*//.*$", "", text) or "{}")


def copilot_home(root, turns, rows=40, cwd=None, long_prompt=(), repeated=()):
    """A COPILOT_HOME under `root` holding one saved chat; returns (folder,
    session id). The signed-in user comes from the real config (the token is
    in the system keyring), and `cwd` is trusted there so no trust question
    shows. Nothing is written to the real folder."""
    home = os.path.join(root, "copilot-home")
    cwd = cwd or os.getcwd()
    sid = str(uuid.uuid4())
    sdir = os.path.join(home, "session-state", sid)
    os.makedirs(sdir, exist_ok=True)
    config = read_config(os.path.join(source_dir(), "config.json"))
    keep = {k: config[k] for k in ("lastLoggedInUser", "loggedInUsers", "firstLaunchAt")
            if k in config}
    keep.update(appTipShown=True, reasoningSummariesCleanupDone=True, trustedFolders=[cwd])
    with open(os.path.join(home, "config.json"), "w") as f:
        json.dump(keep, f, indent=2)
    settings = os.path.join(source_dir(), "settings.json")
    if os.path.exists(settings):
        with open(settings) as f, open(os.path.join(home, "settings.json"), "w") as g:
            g.write(f.read())
    now = datetime.datetime.now(datetime.timezone.utc)

    def stamp(s):
        return (now + datetime.timedelta(seconds=s)).strftime("%Y-%m-%dT%H:%M:%S.000Z")

    events, parent = [], None

    def add(kind, data, at):
        nonlocal parent
        eid = str(uuid.uuid4())
        events.append({"type": kind, "data": data, "id": eid, "timestamp": stamp(at),
                       "parentId": parent})
        parent = eid

    add("session.start", {"sessionId": sid, "version": 1, "producer": "copilot-agent",
                          "copilotVersion": "1.0.98", "startTime": stamp(0), "contextTier": None,
                          "context": {"cwd": cwd}, "alreadyInUse": False,
                          "remoteSteerable": False}, 0)
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
        mid, iid = str(uuid.uuid4()), str(uuid.uuid4())
        add("user.message", {"content": prompt, "transformedContent": prompt, "messageId": mid,
                             "delivery": "idle", "interactionId": iid, "turnId": "0"}, 2 * t + 1)
        add("assistant.turn_start", {"turnId": "0", "interactionId": iid}, 2 * t + 1)
        add("assistant.message", {"messageId": str(uuid.uuid4()), "originatingMessageId": mid,
                                  "model": "gpt-6-luna", "providerId": "copilot",
                                  "content": answer, "toolRequests": [], "interactionId": iid,
                                  "turnId": "0", "phase": "final_answer"}, 2 * t + 2)
        add("assistant.turn_end", {"turnId": "0"}, 2 * t + 2)
    with open(os.path.join(sdir, "events.jsonl"), "w") as f:
        for e in events:
            f.write(json.dumps(e) + "\n")
    with open(os.path.join(sdir, "workspace.yaml"), "w") as f:
        f.write(f"id: {sid}\ncwd: {cwd}\nname: Scroll test\nuser_named: true\n"
                f"summary_count: 0\nfork_count: 0\ncreated_at: {stamp(0)}\n"
                f"updated_at: {stamp(2 * turns)}\n")
    return home, sid
