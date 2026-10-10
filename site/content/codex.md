+++
title = "Explain any words in a Codex CLI answer"
description = "peekme for OpenAI Codex CLI: select text in Codex's output, press Alt+P, and a short explanation opens right under it. Alt+Shift+P asks your own question about it."
weight = 2
template = "agent.html"
[extra]
seo_title = "peekme for Codex CLI: explain selected text in the terminal"
agent = "Codex CLI"
media = "codex"
cmd = "codex"
og = "og/codex.png"
lead = "Select words in Codex's output and press <kbd>Alt+P</kbd> (<kbd>Option+P</kbd> on a Mac). A short explanation opens right under them, and Codex's screen stays as it was. Press <kbd>Alt+Shift+P</kbd> to type your own question about them."
+++

## How it works in Codex

Run `codex` as always. Since Codex 0.157 it draws full screen and makes the selection itself:
drag over text, and peekme reads the selection Codex highlights, also over SSH. Then press Alt+P.

Inside tmux, Codex leaves the mouse to the terminal when tmux has the mouse off, which is tmux's
default. A selection is then your terminal's own, and over SSH peekme cannot read it. Add
`set -g mouse on` to `~/.tmux.conf` and restart Codex. peekme tells you this the first time you
press Alt+P there.

## Which model explains

No API key is needed. peekme asks Codex's own `app-server`, with your Codex login, on a temporary
thread that is not saved, so nothing appears in your history. Codex runs a shared server in the
background, and peekme uses it, so it adds only a few MB of memory. Without that server peekme
starts its own `codex app-server`, which takes about 250 MB.

A peek uses the model your account lists as fast. `/model` in Codex shows the names. A typed question (Alt+Shift+P) uses the
thread's model and gets a copy of the whole thread. To change either model:

```
export PEEKME_MODEL=<model>              # peeks, default your account's fast model
export PEEKME_CODEX_ASK_MODEL=<model>    # questions, default the thread's model
```

## peekme and /side

Codex's own `/side` (also `/btw`) starts a side chat that stays out of the main transcript. It is
good for a question about the work. peekme is for the words in front of you: you select them
instead of typing them, the answer opens right under them, a box can open inside a box, and the
answer stays on the screen as an underline. Use both.
[A closer comparison](@/side-questions.md)

## What gets peekme

`codex`, `codex "prompt"`, `codex resume` and `codex fork` open with peekme. `codex exec` and
anything with output going to a pipe or a file run Codex directly. `command codex` starts plain
Codex once.

Tested with Codex 0.161 on Linux and macOS, also inside tmux and over SSH.
