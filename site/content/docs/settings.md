+++
title = "Settings"
description = "Environment variables for models, agent binaries and the colour of the boxes."
weight = 3
[extra]
nav = "Settings"
lead = "peekme has no config file. Set these environment variables in your shell setup, for example in ~/.bashrc or ~/.zshrc, then start the agent again."
+++

| Variable | Meaning |
|---|---|
| `PEEKME_CLAUDE_MODEL` | model for Claude Code explanations. Default is the chat's model |
| `PEEKME_CLAUDE_BIN` | claude binary to use for explanations. Default is `claude` |
| `PEEKME_MODEL` | model for Codex explanations. Default is the chat's model |
| `PEEKME_CODEX_BIN` | codex binary to use for explanations. Default is `codex` |
| `PEEKME_OWN_SERVER` | set to 1 to start peekme's own `codex app-server` instead of using the shared one |
| `PEEKME_PI_MODEL` | model for Pi explanations, as provider/model. Default is the chat's model |
| `PEEKME_PI_BIN` | pi binary to use for explanations. Default is `pi` |
| `PEEKME_COPILOT_MODEL` | model for Copilot CLI explanations. Default is the chat's model |
| `PEEKME_COPILOT_BIN` | copilot binary to use for explanations. Default is `copilot` |
| `PEEKME_KIRO_MODEL` | model for Kiro CLI explanations. Default is the chat's model |
| `PEEKME_KIRO_BIN` | kiro-cli binary to use for explanations. Default is `kiro-cli` |
| `PEEKME_CLAUDE_ASK_MODEL` | model for typed questions in Claude Code. Default is the chat's model |
| `PEEKME_CODEX_ASK_MODEL` | model for typed questions in Codex. Default is the thread's model |
| `PEEKME_PI_ASK_MODEL` | model for typed questions in Pi, as provider/model. Default is the chat's model |
| `PEEKME_COPILOT_ASK_MODEL` | model for typed questions in Copilot CLI. Default is the chat's model |
| `PEEKME_KIRO_ASK_MODEL` | model for typed questions in Kiro CLI. Default is the chat's model |
| `PEEKME_COLOR` | colour of the boxes in every agent: a hex colour such as `#d77757`, a terminal colour name such as `cyan` or `bright-magenta`, or a 256-colour number such as `173`. Default is the agent's own colour |
| `PEEKME_CLAUDE_COLOR`, `PEEKME_CODEX_COLOR`, `PEEKME_PI_COLOR`, `PEEKME_COPILOT_COLOR`, `PEEKME_KIRO_COLOR` | colour of the boxes in one agent, used instead of `PEEKME_COLOR` |
| `PEEKME_SELECTION` | use this text instead of the mouse selection, for testing |

The boxes take the agent's own colour: orange `#d77757` in Claude Code, the terminal's cyan in
Codex, the terminal's magenta in Pi, purple `#ab7df8` in Copilot CLI and `#c19aff` in Kiro CLI.
A terminal that doesn't say it shows 24-bit colour (`COLORTERM=truecolor`) gets the nearest of its 256 colours instead.

Example:

```
export PEEKME_CLAUDE_MODEL=sonnet
```

## Which model explains

Peek and Ask use your chat's model by default. The model settings above override it separately
for each agent. If the chat's model cannot be read, peekme uses the agent's default.

No API key is needed. peekme uses your existing login through `claude -p`, Codex's `app-server`,
`pi --mode rpc`, Copilot's headless server or `kiro-cli acp`. Explanation sessions have no tools
and are not kept in your agent's chat history.

Explanations use your plan or credits. Nothing starts before the first Alt+P.
