+++
title = "peekme on macOS"
description = "Option+P in Terminal and iTerm2, the clipboard, and SSH from a Mac."
weight = 4
[extra]
nav = "macOS"
lead = "Option+P works as it is, with no terminal setting, also over SSH and inside tmux."
+++

## Option+P without settings

A Mac terminal sends Option+P as the character `π`. When something is selected, peekme takes `π`
as the shortcut. When nothing is selected, `π` is typed as usual, so you never lose the letter.
Option+Shift+P types `∏` and works the same way for questions. If you type Greek, `π` stays a
letter. If your terminal sends Option as Meta, Option+P works too.

## Where the selection comes from

When the agent draws full screen, peekme gets the selection from the agent. That is Claude Code
with `"tui": "fullscreen"`, Codex and Copilot CLI by default, and Kiro CLI after `/fullscreen`.

Otherwise (Claude Code's classic screen, Kiro CLI before `/fullscreen`) peekme reads the
clipboard: iTerm2 copies a selection there by itself, in Terminal press Cmd+C after you select.

## Over SSH

Over SSH only the agent's own selection can be read, so use the full screen mode there. A
selection made with Option held is your terminal's own, and no program on the other side can
read it, so drag without Option. For tmux on the other side, see [tmux and SSH](@/docs/tmux-ssh.md).

Tested in Terminal and iTerm2 with a stand-in agent, and in Terminal with the real Codex, also
inside tmux and over SSH.
