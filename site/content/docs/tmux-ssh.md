+++
title = "tmux and SSH"
description = "Using peekme inside tmux and on a remote machine over SSH."
weight = 5
[extra]
nav = "tmux and SSH"
lead = "peekme runs where the agent runs. Install it on the machine you SSH into, and it works inside tmux there."
+++

## Over SSH

In full screen mode the agent makes the selection itself, and peekme reads it from the agent, so
it works over SSH with no extra setup. Full screen is Claude Code with `"tui": "fullscreen"`,
Codex and Copilot CLI by default, and Kiro CLI after `/fullscreen`.

On the classic screen the selection belongs to your local terminal, and a program on the other
side of SSH can't read it.

## Inside tmux

With tmux's mouse on, a drag over text the agent doesn't take is a tmux selection, and Alt+P
explains it, also when your tmux stays in copy mode after the drag.

Codex leaves the mouse to the terminal when tmux has the mouse off, which is tmux's default. A
selection is then your terminal's own, and over SSH peekme cannot read it. Add this to
`~/.tmux.conf` and restart Codex:

```
set -g mouse on
```

peekme tells you this the first time you press Alt+P there.
