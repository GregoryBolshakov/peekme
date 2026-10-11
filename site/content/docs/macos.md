+++
title = "peekme on macOS"
description = "Option+P in Terminal and iTerm2, where the selection comes from, and SSH from a Mac."
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

Otherwise (Claude Code's classic screen, Kiro CLI before `/fullscreen`) the selection is your
terminal's own:

- iTerm2 copies a selection to the clipboard by itself, and peekme reads it there.
- Terminal keeps it to itself. The first Option+P there offers to read it through macOS
  Accessibility: press Enter, then turn on Terminal in System Settings > Privacy & Security >
  Accessibility. It is needed once. If Option+P still types `π`, quit Terminal and open it again.
  `peekme doctor` says if it is on. Esc instead keeps things as before, and Cmd+C after you
  select works too.

## Over SSH

Over SSH the terminal's selection stays on your Mac. Use the agent's full screen mode there, or
run the agent inside tmux with its mouse on: then tmux makes the selection on the other side,
and a click on underlined text works too. See [tmux and SSH](@/docs/tmux-ssh.md).

In iTerm2 a drag with Option held is always iTerm2's own selection, so drag without Option there.

Tested in Terminal and iTerm2 with a stand-in agent, and in Terminal with the real Codex, also
inside tmux and over SSH.
