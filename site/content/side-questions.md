+++
title = "/btw, /side, /ask and peekme"
description = "Claude Code /btw, Codex /side, Copilot CLI /ask and Kiro tangent mode answer side questions. peekme explains the words you select, right under them. How they differ."
weight = 10
[extra]
seo_title = "/btw, /side, /ask or peekme: side questions in coding agents"
og = "og/side-questions.png"
lead = "Every coding agent now has a way to ask a side question without adding it to the conversation. peekme does something next to that: it explains the words you are looking at, in place. Here is when each one fits."
+++

## The built-in side questions

| Agent | Command | How you ask | Where the answer opens |
|---|---|---|---|
| Claude Code | `/btw` | type `/btw` and the question | an overlay over the screen |
| Codex CLI | `/side` (also `/btw`) | type `/side` | a side chat, outside the main transcript |
| GitHub Copilot CLI | `/ask` (also `/btw`) | type `/ask` and the question | a separate dialog |
| Kiro CLI | tangent mode (experimental) | enter a tangent, ask, return | in the chat, then you go back to where you were |

All of them answer from the conversation and keep the question out of its history. Claude Code's
`/btw` and Copilot's `/ask` work while the agent is still busy, and they have no tools. See
[Claude Code](https://code.claude.com/docs/en/interactive-mode),
[Codex](https://developers.openai.com/codex/cli/slash-commands) and
[Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/ask-a-side-question)
docs.

## What peekme does differently

- **You point instead of typing.** Select the words and press Alt+P. You don't retype or copy a
  long term, a path or an error message into a question.
- **The answer opens next to the words.** The lines below move down to make room, like Peek in
  VS Code. You read the explanation and the text it explains together.
- **A short peek is fast.** Alt+P goes to a small, fast model with only the parts of the
  conversation around the selection, usually 1 to 7 KB. The answer starts in 1 to 2 seconds.
- **Boxes inside boxes.** Select words in an answer and press Alt+P again. A new box opens inside,
  under that line, and you can go several levels deep.
- **Answers stay.** Text you asked about gets a dotted underline. Click it later and the answer
  opens again with no new request.
- **Same keys in four agents.** If you switch between Claude Code, Codex, Copilot CLI and Kiro
  CLI, you learn one thing.

For your own question about the words, press Alt+Shift+P. It goes to the model your chat runs on,
with the whole conversation, like a side question does, and opens in the same place.

## When to use which

Use the built-in side question for a question about the work: "what was that config file
called?", "why did you pick this approach?". Use peekme when a word or a line in the answer
stops you: a term you don't know, a flag, a library name, an error. Most people will use both.
