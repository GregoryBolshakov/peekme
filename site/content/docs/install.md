+++
title = "Install peekme"
description = "npm, Homebrew, a shell script or Cargo, then peekme install."
weight = 1
[extra]
nav = "Install"
lead = "Two commands, then open a new terminal."
+++

## Install

```
npm i -g peekme
peekme install
```

Open a new terminal. Now `claude`, `codex`, `copilot` and `kiro-cli` open with peekme, and you
type them as always. That's all.

Don't have npm? Use one of these instead of the first line:

- Homebrew: `brew install gregorybolshakov/tap/peekme`
- Rust: `cargo install peekme`
- Neither: `curl -LsSf https://github.com/GregoryBolshakov/peekme/releases/latest/download/peekme-installer.sh | sh`

Ready binaries for Linux and macOS (x86_64 and ARM) are on the
[latest release](https://github.com/GregoryBolshakov/peekme/releases/latest) page. npm 12 may say
that it skipped an install script of peekme. That is fine: peekme then downloads its binary the
first time it runs.

peekme has no account of its own. The explanations use the login of the agent you are in, so you
need at least one of Claude Code, Codex CLI, GitHub Copilot CLI or Kiro CLI, logged in.

## What `peekme install` does

1. It defines shell functions `claude`, `codex`, `copilot` and `kiro-cli` in your shell setup. A
   function wins over anything on PATH, so it keeps working when nvm, mise, Homebrew or an
   agent's installer change PATH later. Aliases that call them go through it too.
2. It puts `claude`, `codex`, `copilot` and `kiro-cli` links to peekme in
   `~/.local/share/peekme/bin` at the front of PATH, so scripts that run them also get peekme. If
   some tool puts its own directory in front of it later, only scripts lose peekme, and
   `peekme doctor` tells you which directory it is.

Both find the real agent on PATH themselves, so updating the agent changes nothing. If you remove
peekme, the functions fall back to the plain agents.

The block in `~/.bashrc` or `~/.zshrc` (for fish, a file in `~/.config/fish/conf.d`) is short and
marked. It covers all four agents, so an agent you install later gets peekme too. When a new
peekme version knows more agents, it adds them to your setup the next time it starts and says so
once.

## Check and remove

```
peekme doctor      # check the setup, and see what is in the way if something is
peekme uninstall   # remove it, your files are as before
command claude     # run plain Claude Code once (same for codex, copilot, kiro-cli)
```

## What gets peekme

Only the interactive screen gets peekme: `claude`, `claude "prompt"`, `claude --resume`,
`claude -c`, `codex`, `codex "prompt"`, `codex resume`, `codex fork`, `copilot`,
`copilot -i "prompt"`, `copilot --resume`, and `kiro-cli`, `kiro-cli chat`, `kiro-cli --resume`.
Commands like `claude -p`, `codex exec`, `copilot -p`, `copilot login`,
`kiro-cli chat --no-interactive` or `kiro-cli acp`, and anything with output going to a pipe or
a file, run the agent directly.

To use peekme with another program, run `peekme -- COMMAND`.
