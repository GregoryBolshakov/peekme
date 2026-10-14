#!/usr/bin/env python3
"""A Codex chat with real prompts and answers, for tests that need Codex to
draw a conversation without asking a model: a saved session written to a
Codex home of its own, opened with `codex resume`.

Each turn is a prompt and an answer of labelled rows (`b<turn>r<row>`), as a
list, so every row stays a row. Codex pins the prompt of the answer at the top
of the screen while reading it (since 0.162), which shell commands (`!cmd`)
never show.
"""
import datetime
import json
import os
import shutil
import subprocess
import uuid

HOME = os.path.expanduser("~/.codex")


def forget_sign_in(home):
    """Remove the copy of the sign-in from a home made by `codex_home`."""
    path = os.path.join(home, "auth.json")
    if os.path.exists(path):
        subprocess.run(["shred", "-u", path], check=False)
        if os.path.exists(path):
            os.remove(path)


def codex_home(root, turns, rows=40, cwd=None, long_prompt=()):
    """A Codex home under `root` holding one saved chat; returns (home, id).
    Settings and sign-in come from the real home, nothing is written there;
    call `forget_sign_in` when done."""
    home = os.path.join(root, "codex-home")
    os.makedirs(home, exist_ok=True)
    for name in ("config.toml", "auth.json"):
        src = os.path.join(HOME, name)
        if os.path.exists(src):
            shutil.copy(src, os.path.join(home, name))
    sid = str(uuid.uuid4())
    now = datetime.datetime.now(datetime.timezone.utc)
    stamp = now.strftime("%Y-%m-%dT%H:%M:%S.") + f"{now.microsecond // 1000:03d}Z"
    day = os.path.join(home, "sessions", now.strftime("%Y/%m/%d"))
    os.makedirs(day, exist_ok=True)
    path = os.path.join(day, f"rollout-{now.strftime('%Y-%m-%dT%H-%M-%S')}-{sid}.jsonl")
    recs = [{"type": "session_meta", "payload": {
        "id": sid, "session_id": sid, "timestamp": stamp, "cwd": cwd or os.getcwd(),
        "originator": "codex-tui", "cli_version": "0.162.1", "source": "cli",
        "model_provider": "openai", "base_instructions": {"text": ""},
        "history_mode": "paginated", "multi_agent_version": "v1"}}]
    ms = int(now.timestamp() * 1000)
    for t in range(turns):
        turn = f"turn-{t}"
        prompt = f"q{t} show block {t}"
        if t in long_prompt:
            prompt += " " + " ".join(f"and some more words of the prompt {i}" for i in range(12))
        answer = "\n".join(f"- b{t}r{i:02d} words of row {i} in block {t}"
                           for i in range(1, rows + 1))
        recs += [
            {"type": "event_msg", "payload": {"type": "task_started", "turn_id": turn,
                                              "started_at": int(now.timestamp()),
                                              "model_context_window": None,
                                              "collaboration_mode_kind": "default"}},
            {"type": "event_msg", "payload": {
                "type": "item_completed", "thread_id": sid, "turn_id": turn,
                "item": {"type": "UserMessage", "id": f"u{t}",
                         "content": [{"type": "text", "text": prompt, "text_elements": []}]},
                "completed_at_ms": ms}},
            {"type": "response_item", "payload": {
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": prompt}]}},
            {"type": "event_msg", "payload": {
                "type": "item_completed", "thread_id": sid, "turn_id": turn,
                "item": {"type": "AgentMessage", "id": f"a{t}",
                         "content": [{"type": "Text", "text": answer}]},
                "completed_at_ms": ms}},
            {"type": "response_item", "payload": {
                "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": answer}]}},
            {"type": "event_msg", "payload": {"type": "task_complete", "turn_id": turn,
                                              "last_agent_message": answer,
                                              "started_at": int(now.timestamp())}},
        ]
    with open(path, "w") as f:
        for i, r in enumerate(recs):
            f.write(json.dumps({"timestamp": stamp, "ordinal": i, **r}) + "\n")
    return home, sid
