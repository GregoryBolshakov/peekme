+++
title = "Explain any words in a Claude Code answer"
description = "peekme for Claude Code: select text in Claude Code's output, press Alt+P, and a short explanation opens right under it. Alt+Shift+P asks your own question about it."
weight = 1
template = "agent.html"
[extra]
seo_title = "peekme for Claude Code: explain selected text in the terminal"
agent = "Claude Code"
media = "claude"
cmd = "claude"
og = "og/claude-code.png"
lead = "Select words in Claude Code's output and press <kbd>Alt+P</kbd> (<kbd>Option+P</kbd> on a Mac). A short explanation opens right under them, and Claude's screen stays as it was. Press <kbd>Alt+Shift+P</kbd> to type your own question about them."
+++

## How it works in Claude Code

Run `claude` as always. Select text with the mouse and press Alt+P.

- **Full screen** (`"tui": "fullscreen"` in your Claude Code settings): Claude makes the
  selection itself and peekme reads it, also over SSH. A click on underlined text opens the
  saved answer. Keep `copyOnSelect` on. Without it Claude doesn't report the selection, and only
  a Shift+drag selection works.
- **Classic screen:** the selection is your terminal's own. peekme reads it from X11 PRIMARY on
  Linux and from the clipboard on a Mac (iTerm2 copies a selection there by itself, in Terminal
  press Cmd+C after you select). Over SSH use the full screen mode.

In Claude Code, Alt+P opens the model picker. With peekme it explains the selection instead.
`/model` still opens the model picker.

## Which model explains

No API key is needed. A peek runs `claude -p` with your own Claude login, on Haiku, without tools
and without saving the session, so nothing appears in `/resume`. After the first explanation one
`claude -p` waits in the background for the next one, so it starts in about half a second. It
takes about 210 MB of memory.

A typed question (Alt+Shift+P) goes to the model your chat runs on, with the whole
conversation. To change either model:

```
export PEEKME_CLAUDE_MODEL=sonnet        # peeks, default haiku
export PEEKME_CLAUDE_ASK_MODEL=opus      # questions, default the chat's model
```

peekme knows the conversation from Claude Code's session transcript in `~/.claude/projects`. It
finds the right session from the process id Claude registers there.

## peekme and /btw

Claude Code's own `/btw` answers a typed side question in an overlay, from the conversation,
without adding it to the history. It is good for a question about the work. peekme is for the
words in front of you: you select them instead of typing them, the answer opens right under them,
a box can open inside a box, and the answer stays on the screen as an underline. Use both.
[A closer comparison](@/side-questions.md)

## What gets peekme

`claude`, `claude "prompt"`, `claude --resume` and `claude -c` open with peekme. `claude -p` and
anything with output going to a pipe or a file run Claude directly. `command claude` starts plain
Claude Code once.

Tested with Claude Code 2.1.292 on Linux, also inside tmux and over SSH.
