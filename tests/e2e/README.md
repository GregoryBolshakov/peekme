# End-to-end tests

peekme sits between a terminal and an agent CLI, often with tmux and SSH in
between. Each layer can change how keys, mouse events and the agent's
clipboard writes travel. These tests run peekme through those layers.

## Linux: the matrix (every release)

```
cargo build --release --bins --examples
python3 tests/e2e/matrix.py           # fake agents, 84 cases, about a minute
python3 tests/e2e/real_agents.py      # real claude, codex, copilot, 34 cases
```

Layers: direct, tmux, ssh, ssh+tmux, tmux+ssh+tmux. tmux runs with
`mouse on|off` and `extended-keys on|off`. The script plays the terminal and
sends what a terminal sends for the shortcut: `π` (a Mac terminal with Option
not set as Meta) or `ESC p` (Option as Meta, or Alt on Linux), and SGR mouse
reports for a drag.

`fakeagent.py` behaves like the real agents toward the terminal: full screen,
mouse on, the same input box, and it copies a drag the way each agent does
(OSC 52 for Claude Code and Copilot, reverse video for Codex, and inside tmux
`tmux load-buffer` for Claude Code and Codex). `real_agents.py` uses the real
CLIs and drags over their startup screen.

Nothing is sent to a model: `PEEKME_FAKE_EXPLAINER=1` makes peekme answer with
canned text. `PEEKME_EVENT_LOG` records what peekme decided, and the fake agent
logs every byte it gets, so a test can tell whether a key was taken by peekme
or reached the agent.

The SSH layers start a private `sshd` on 127.0.0.1 as the current user. Set
`E2E_TMUX`, `E2E_SSHD` and `E2E_LD_LIBRARY_PATH` to use binaries that are not
on PATH. Run `real_agents.py` from a directory Claude Code already trusts.

## macOS: real Terminal and iTerm2

`.github/workflows/macos-e2e.yml` runs on a GitHub macOS runner. It opens
Terminal and iTerm2, in their default settings and with Option as Meta, and
drives them with real key and mouse events.
