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

That means peekme saw no selection. On the classic screen of Claude Code or Kiro CLI, press Cmd+C
in Terminal after you select. Over SSH, use the agent's full screen mode. See
[macOS](@/docs/macos.md).

## Codex inside tmux over SSH

Turn tmux's mouse on with `set -g mouse on` and restart Codex. See [tmux and SSH](@/docs/tmux-ssh.md).

## Claude Code full screen ignores the selection

If `copyOnSelect` is off, Claude does not report the selection. Turn it on, or select with
Shift+drag.

## Known limits

- Only text that is on screen now. Text that scrolled up into the terminal history can't be
  selected yet.
- Outside full screen mode peekme needs the terminal's selection, which it reads on Linux with
  X11 only. On Wayland it works through XWayland, and on macOS it reads the clipboard.
- A click on underlined text works only when the agent takes the mouse (full screen mode).
- An explanation takes about 1 to 2 seconds to start arriving.
- No Windows yet.

## Report a bug

Open an issue on [GitHub](https://github.com/GregoryBolshakov/peekme/issues/new/choose) with the
output of `peekme doctor`, your agent and its version, your terminal, and what you did.
