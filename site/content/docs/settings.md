+++
title = "Settings"
description = "Environment variables for models and agent binaries."
weight = 3
[extra]
nav = "Settings"
lead = "peekme has no config file. Set these environment variables in your shell setup, for example in ~/.bashrc or ~/.zshrc, then start the agent again."
+++

| Variable | Meaning |
|---|---|
| `PEEKME_CLAUDE_MODEL` | model for Claude Code explanations. Default is `haiku` |
| `PEEKME_CLAUDE_BIN` | claude binary to use for explanations. Default is `claude` |
| `PEEKME_MODEL` | model for Codex explanations. Default is the model your account lists as fast |
| `PEEKME_CODEX_BIN` | codex binary to use for explanations. Default is `codex` |
| `PEEKME_OWN_SERVER` | set to 1 to start peekme's own `codex app-server` instead of using the shared one |
| `PEEKME_COPILOT_MODEL` | model for Copilot CLI explanations. Default is Copilot's Auto |
| `PEEKME_COPILOT_BIN` | copilot binary to use for explanations. Default is `copilot` |
| `PEEKME_KIRO_MODEL` | model for Kiro CLI explanations. Default is `claude-haiku-4.5` |
| `PEEKME_KIRO_BIN` | kiro-cli binary to use for explanations. Default is `kiro-cli` |
| `PEEKME_CLAUDE_ASK_MODEL` | model for typed questions in Claude Code. Default is the chat's model |
| `PEEKME_CODEX_ASK_MODEL` | model for typed questions in Codex. Default is the thread's model |
| `PEEKME_COPILOT_ASK_MODEL` | model for typed questions in Copilot CLI. Default is the chat's model |
| `PEEKME_KIRO_ASK_MODEL` | model for typed questions in Kiro CLI. Default is the chat's model |
| `PEEKME_SELECTION` | use this text instead of the mouse selection, for testing |

Example:

```
export PEEKME_CLAUDE_MODEL=sonnet
```

## Which model explains

No API key is needed. peekme asks the agent's own CLI, with your existing login:

- **Claude Code:** `claude -p` on Haiku, without tools and without saving the session, so
  nothing appears in `/resume`. After the first explanation one `claude -p` waits in the
  background for the next one, so it starts in about half a second. It takes about 210 MB.
- **Codex:** its `app-server`, on a temporary thread that is not saved. Since Codex 0.157 a shared
  Codex server runs in the background, and peekme uses it, so it adds only a few MB of memory.
  Without that server it starts its own `codex app-server`, which takes about 250 MB.
- **Copilot CLI:** its headless server (`copilot --headless`, the one the Copilot SDK uses),
  started on the first explanation. Each explanation is a short session without tools, and
  peekme deletes it afterwards. The model is Copilot's Auto unless you set one. It uses a little
  of your AI credits for each explanation.
- **Kiro CLI:** its ACP server (`kiro-cli acp`), started on the first explanation. It runs with a
  peekme agent that has no tools. Each explanation is a short session, and peekme deletes it
  afterwards. The model is `claude-haiku-4.5` unless you set one (Auto if your plan doesn't have
  it). One explanation costs about 0.01 Kiro credits.

It does use your plan, a little for each explanation. Nothing starts before the first Alt+P.
