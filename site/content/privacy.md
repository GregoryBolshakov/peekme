+++
title = "Privacy"
description = "What text peekme sends, to which model, and on whose login. No peekme server, no telemetry."
weight = 30
[extra]
lead = "peekme has no server and no account. It sends text only when you press Alt+P or Alt+Shift+P, and only through the agent you are already using."
+++

## What is sent, and where

An explanation goes through your agent's own CLI, with your own login: `claude -p` for Claude
Code, Codex's app-server, Copilot's headless server, or `kiro-cli acp`. So the text goes to the
same company your agent already sends your chat to (Anthropic, OpenAI, GitHub or AWS), under the
same terms. Nothing goes to peekme or to anyone else.

For a peek (Alt+P) the model gets:

1. One line saying which agent this is and in which directory.
2. The text around the selection, with the selection marked in place.
3. Your request that this text answers, and one line for each of your earlier requests.
4. The first places in the conversation where the same words were used before.

That is usually 1 to 7 KB. Alt+P again sends the whole conversation, and above about 40k tokens
peekme shows the size first and sends it only after one more Alt+P. A typed question
(Alt+Shift+P) sends the whole conversation to the model your chat runs on.

## What stays on your machine

- peekme reads the conversation from the agent's own session files on your disk, or from Codex's
  app-server.
- The sessions peekme uses for explanations are not saved or are deleted afterwards, so they
  don't appear in your agent's session list.
- Saved answers (the dotted underlines) live in memory and are gone when the agent exits.
- peekme writes errors (not text from your screen) to `~/.local/state/peekme/peekme.log`. For
  Kiro it keeps its no-tools agent file in a temporary directory.
- peekme has no telemetry and makes no network requests of its own. Only the npm package
  downloads the peekme binary from GitHub, once, when it is installed or first run.

## This website

peekme.dev sets no cookies and has no analytics or tracking. It is a static site hosted on
GitHub Pages, which keeps its own server logs. The star button asks the GitHub API for the star
count.
