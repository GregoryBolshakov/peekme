+++
title = "Troubleshooting"
description = "peekme doctor, and what to check when Alt+P does nothing."
weight = 6
[extra]
nav = "Troubleshooting"
lead = "Start with peekme doctor. It checks the setup and says what is in the way."
+++

```
peekme doctor
```

## Alt+P does nothing

- Open a new terminal after `peekme install`. The shell setup is read when a shell starts.
- Run `type claude` (or `codex`, `copilot`, `kiro-cli`). It should say it is a function. If it
  is not, run `peekme install` again and look at the output of `peekme doctor`.
- If a box opens and says "Select some text with the mouse first", peekme saw no selection.
  The box says why when it knows, for example tmux's mouse being off over SSH.

## On a Mac, Option+P types π

That means peekme saw no selection. On the classic screen of Claude Code or Kiro CLI in
Terminal, the first Option+P offers to read the selection through macOS Accessibility. Press
Enter and turn on Terminal in System Settings > Privacy & Security > Accessibility. If you
pressed Esc, Cmd+C after you select works too. Over SSH, use the agent's full screen mode or tmux
with its mouse on. See [macOS](@/docs/macos.md) and [tmux and SSH](@/docs/tmux-ssh.md).

## Codex inside tmux over SSH

Turn tmux's mouse on with `set -g mouse on` and restart Codex. See [tmux and SSH](@/docs/tmux-ssh.md).

## Claude Code full screen ignores the selection

If `copyOnSelect` is off, Claude does not report the selection. Turn it on, or select with
Shift+drag.

## Known limits

- Text you select up in the terminal's history works only while the agent draws inline (Kiro
  CLI, Claude Code's classic screen), for the last 2000 lines. Its box opens at the top of the
  screen, without an underline. In full screen, scroll the agent's chat to bring the text back.
- Outside full screen mode peekme needs the terminal's selection (or tmux's), which it reads on
  Linux with X11 only. On Wayland it works through XWayland, and on macOS it reads the clipboard
  or, in Terminal, asks macOS Accessibility.
- A click on underlined text works when the agent takes the mouse (full screen mode), or inside
  tmux with its mouse on.
- Response time depends on the model and how much context is sent.
- No Windows yet.

## Report a bug

Open an issue on [GitHub](https://github.com/GregoryBolshakov/peekme/issues/new/choose) with the
output of `peekme doctor`, your agent and its version, your terminal, and what you did.
