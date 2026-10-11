+++
title = "tmux and SSH"
description = "Using peekme inside tmux and on a remote machine over SSH: who takes the mouse, and what peekme can read."
weight = 5
[extra]
nav = "tmux and SSH"
lead = "peekme runs where the agent runs. Install it on the machine you SSH into, and it works inside tmux there."
+++

## Who takes the mouse

What peekme can read depends on who takes the mouse: the agent, tmux, or your terminal.

| Agent | tmux mouse on (`set -g mouse on`) | tmux mouse off (tmux's default) |
|---|---|---|
| Full screen: Claude Code with `"tui": "fullscreen"`, Copilot CLI, Kiro CLI after `/fullscreen` | the agent's selection, also over SSH | the same |
| Codex | the agent's selection, also over SSH | your terminal's selection (Codex leaves it the mouse), not readable over SSH |
| Claude Code classic screen, Kiro CLI inline | tmux's selection, also over SSH | your terminal's selection, not readable over SSH |

## Over SSH

In full screen mode the agent makes the selection itself, and peekme reads it from the agent, so
it works over SSH with no extra setup. Full screen is Claude Code with `"tui": "fullscreen"`,
Codex and Copilot CLI by default, and Kiro CLI after `/fullscreen`.

On Claude Code's classic screen and Kiro CLI inline the selection belongs to your local
terminal, and a program on the other side of SSH can't read it. Run the agent inside tmux with
its mouse on instead, and tmux makes the selection on the remote machine.

## Inside tmux

With `set -g mouse on` and an agent that leaves the mouse alone, a drag is tmux's selection.
Alt+P and Alt+Shift+P explain it, also when your tmux stays in copy mode after the drag, and a
click on underlined text opens the saved answer (tmux 3.2 or newer). Drags and the wheel stay
with tmux.

For this peekme adds key bindings to tmux that act only in its own pane: Option+P and
Option+Shift+P in copy mode, and a plain click. Keys you bound yourself are left alone.

Codex asks tmux at startup whether it has the mouse. With tmux's mouse off, which is tmux's
default, Codex leaves the mouse to the terminal. Add this to `~/.tmux.conf` and restart Codex:

```
set -g mouse on
```

peekme tells you this the first time you press Alt+P there.

## From a Mac

In iTerm2 a drag with Option held is always iTerm2's own selection, and over SSH no program can
read it, so drag without Option. See [macOS](@/docs/macos.md).
