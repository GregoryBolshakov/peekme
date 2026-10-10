+++
title = "Explain any words in a Kiro CLI answer"
description = "peekme for Kiro CLI: select text in Kiro's output, press Alt+P, and a short explanation opens right under it. Alt+Shift+P asks your own question about it."
weight = 4
template = "agent.html"
[extra]
seo_title = "peekme for Kiro CLI: explain selected text in the terminal"
agent = "Kiro CLI"
media = "kiro"
cmd = "kiro-cli"
og = "og/kiro-cli.png"
lead = "Select words in Kiro CLI's output and press <kbd>Alt+P</kbd> (<kbd>Option+P</kbd> on a Mac). A short explanation opens right under them, and Kiro's screen stays as it was. Press <kbd>Alt+Shift+P</kbd> to type your own question about them."
+++

## How it works in Kiro CLI

Run `kiro-cli` as always. Kiro starts inline and leaves the mouse to your terminal, so peekme
reads your terminal's selection (X11 PRIMARY on Linux, the clipboard on a Mac). After
`/fullscreen` Kiro makes the selection itself and peekme reads it, also over SSH. Underlined text
can be clicked only in full screen.

## Which model explains

No API key is needed. On the first explanation peekme starts Kiro's ACP server (`kiro-cli acp`,
the one Kiro's own screen and editors use) with your Kiro login. It runs with a peekme agent that
has no tools. Each explanation is a short session, and peekme deletes it afterwards, so nothing
appears in your session list. One explanation costs about 0.01 Kiro credits.

A peek uses `claude-haiku-4.5` (Auto if your plan doesn't have it). A typed question
(Alt+Shift+P) uses the model your chat runs on, with the whole conversation. To change either
model:

```
export PEEKME_KIRO_MODEL=<model>        # peeks, default claude-haiku-4.5
export PEEKME_KIRO_ASK_MODEL=<model>    # questions, default the chat's model
```

peekme knows the conversation from Kiro's session log in `~/.kiro/sessions/cli`.

## peekme and tangent mode

Kiro CLI has an experimental tangent mode for side topics that you can leave and return to the
main conversation. It is good for a question about the work. peekme is for the words in front of
you: you select them instead of typing them, the answer opens right under them, a box can open
inside a box, and the answer stays on the screen as an underline.
[A closer comparison](@/side-questions.md)

## What gets peekme

`kiro-cli`, `kiro-cli chat` and `kiro-cli --resume` open with peekme.
`kiro-cli chat --no-interactive`, `kiro-cli acp` and anything with output going to a pipe or a
file run Kiro directly. `command kiro-cli` starts plain Kiro CLI once.

Tested with Kiro CLI 2.28 on Linux, also inside tmux and over SSH.
