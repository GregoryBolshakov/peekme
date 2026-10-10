+++
title = "Explain any words in a GitHub Copilot CLI answer"
description = "peekme for GitHub Copilot CLI: select text in Copilot's output, press Alt+P, and a short explanation opens right under it. Alt+Shift+P asks your own question about it."
weight = 3
template = "agent.html"
[extra]
seo_title = "peekme for GitHub Copilot CLI: explain selected text in the terminal"
agent = "GitHub Copilot CLI"
media = "copilot"
cmd = "copilot"
og = "og/copilot-cli.png"
lead = "Select words in Copilot CLI's output and press <kbd>Alt+P</kbd> (<kbd>Option+P</kbd> on a Mac). A short explanation opens right under them, and Copilot's screen stays as it was. Press <kbd>Alt+Shift+P</kbd> to type your own question about them."
+++

## How it works in Copilot CLI

Run `copilot` as always. Copilot CLI draws full screen and makes the selection itself: drag over
text, and peekme reads it, also over SSH. Then press Alt+P. A click on underlined text opens the
saved answer.

## Which model explains

No API key is needed. On the first explanation peekme starts Copilot's headless server
(`copilot --headless`, the one the Copilot SDK uses) with your GitHub login. Each explanation is
a short session without tools, and peekme deletes it afterwards, so nothing appears in your
session list. It uses a little of your AI credits for each explanation.

A peek uses Copilot's Auto model. A typed question (Alt+Shift+P) uses the model your chat runs
on, with the whole conversation. To change either model:

```
export PEEKME_COPILOT_MODEL=<model>        # peeks, default Auto
export PEEKME_COPILOT_ASK_MODEL=<model>    # questions, default the chat's model
```

`/model` in Copilot CLI shows the names. peekme knows the conversation from Copilot's session log
in `~/.copilot/session-state`.

## peekme and /ask

Copilot CLI's own `/ask` (also `/btw`) answers a typed side question in a separate dialog,
without adding it to the history. It is good for a question about the work. peekme is for the
words in front of you: you select them instead of typing them, the answer opens right under them,
a box can open inside a box, and the answer stays on the screen as an underline. Use both.
[A closer comparison](@/side-questions.md)

## What gets peekme

`copilot`, `copilot -i "prompt"` and `copilot --resume` open with peekme. `copilot -p`,
`copilot login` and anything with output going to a pipe or a file run Copilot directly.
`command copilot` starts plain Copilot CLI once.

Tested with Copilot CLI 1.0.93 on Linux, also inside tmux and over SSH.
