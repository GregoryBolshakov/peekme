+++
title = "Explain any words in a Pi answer"
description = "peekme for the Pi coding agent: select text in Pi's output, press Alt+P, and a short explanation opens right under it. Alt+Q asks your own question about it."
weight = 3
template = "agent.html"
[extra]
seo_title = "peekme for Pi: explain selected text in the terminal"
agent = "Pi"
cmd = "pi"
lead = "Select words in Pi's output and press <kbd>Alt+P</kbd> (<kbd>Option+P</kbd> on a Mac). A short explanation opens right under them, and Pi's screen stays as it was. Press <kbd>Alt+Q</kbd> to type your own question about them."
+++

## How it works in Pi

Run `pi` as always. Pi draws full screen by default and makes the selection itself: drag over
text, Pi highlights it and copies it, and peekme reads that copy, also inside tmux and over SSH.
Then press Alt+P. peekme starts Pi with `PI_CLIPBOARD_OSC52=1`, so Pi also sends each copy to
your terminal the way it does over SSH. Most terminals put that on the clipboard too.

On Pi's regular screen (`pi --tui-mode regular`, or `tuiMode` in its settings) Pi leaves the
mouse to your terminal, so peekme reads your terminal's selection: X11 PRIMARY on Linux, on a Mac
the clipboard in iTerm2 and macOS Accessibility in Terminal. Inside tmux with its mouse on, tmux
makes the selection and peekme reads it from tmux, also over SSH. See
[tmux and SSH](@/docs/tmux-ssh.md).

## Scrolling with a box open

On the full screen, scroll as usual. The chat and the box move together, as far as Pi scrolls on
its own: a row for a slow turn of the wheel, more when you turn it fast, or the rows you set in
`fullscreenWheelScrollLines`. PgUp and PgDn move a screen less 4 rows. Pi's scrollbar and its
"Jump to latest message" hint stay where Pi draws them. Ctrl+End jumps to the end of the chat, as
in Pi, and closes the box.

On the regular screen peekme scrolls the chat with the box in it itself: 3 rows a turn of the
wheel, a page for PgUp and PgDn. It goes up to the oldest line peekme saw in your terminal's
history and down to the end of the chat. A long answer, and a box inside it, are as tall as their
text and never scroll on their own.

## Which model explains

No API key of peekme's own is needed. On the first explanation peekme starts Pi in RPC mode
(`pi --mode rpc`) with the providers you logged in to in Pi. It runs in a folder of its own,
with no tools, extensions, skills or context files, and keeps no session on disk, so nothing
appears in `/resume`. Explanations use your provider's plan or credits.

Peek and Ask use your chat's model by default. Peek sends relevant parts of the conversation.
Ask sends the whole conversation. To change either model, give it as provider/model:

```
export PEEKME_PI_MODEL=<provider>/<model>       # peeks
export PEEKME_PI_ASK_MODEL=<provider>/<model>   # questions
```

peekme knows the conversation from Pi's session file in `~/.pi/agent/sessions` (or under
`PI_CODING_AGENT_DIR`), the branch you see on the screen. Two Pi chats in the same folder at the
same time can give the explanation the other chat's context.

## peekme and side questions

Pi has no side question command of its own. peekme is for the words in front of you: you select
them instead of typing them, the answer opens right under them, a box can open inside a box, and
the answer stays on the screen as an underline. [A closer comparison](@/side-questions.md)

## What gets peekme

`pi`, `pi "message"`, `pi -c`, `pi -r` and `pi --session <file>` open with peekme. `pi -p`,
`pi --mode json`, `pi --mode rpc`, `pi --export`, commands like `pi install`, `pi update`,
`pi config` and `pi auth`, and anything with output going to a pipe or a file run Pi directly.
`command pi` starts plain Pi once.

Some Linux systems have another program called `pi` that prints the digits of pi. peekme leaves
it alone, and `peekme doctor` says which one it found.

Tested with Pi 1.4 on Linux, also inside tmux and over SSH.
