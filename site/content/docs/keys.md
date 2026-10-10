+++
title = "Keys"
description = "Alt+P, Alt+Shift+P, Esc, clicks and scrolling in peekme."
weight = 2
[extra]
nav = "Keys"
lead = "Select text with the mouse as you normally do, then press Alt+P. On a Mac it is Option+P."
+++

| Key | When | What it does |
|---|---|---|
| Alt+P (Mac: Option+P) | text is selected | explain it |
| Alt+Shift+P (Mac: Option+Shift+P) | text is selected | type a question about it first, Enter sends it |
| Alt+P again | box is open, same selection | explain again using the whole conversation |
| Alt+P or Alt+Shift+P on text in the box | box is open | open a new box inside it, under that line |
| click on underlined text | any time | show the saved answer, with no new request |
| PgUp / PgDn, mouse wheel | box is open | scroll a long answer |
| Esc | box is open | close the innermost box |
| typing | box is open | goes to the agent's input line, the box stays open |
| Enter | box is open | sends your message and closes the box |

A second click on underlined text closes the saved answer. Boxes inside boxes go several levels
deep while the window is wide enough. The wheel scrolls the box under the pointer.

When the agent draws full screen (Claude Code with `"tui": "fullscreen"`, Codex since 0.157,
Copilot CLI, Kiro CLI after `/fullscreen`), it makes the selection itself, and peekme uses that
selection. A click on underlined text works only there.

In Claude Code, Alt+P opens the model picker. With peekme it explains the selection instead.
`/model` still opens the model picker.

## Ask a question

After Alt+Shift+P the box opens empty and you type. Enter sends the question, Esc cancels. While
you type, the keys stay in the box and don't go to the agent.

The question goes to the model your chat runs on, not the small one, together with the whole
conversation (Codex gets a copy of the thread). So it takes longer and costs more than a peek.
To use another model, see [Settings](@/docs/settings.md).

- With a box open and nothing new selected, Alt+Shift+P asks about the text that box explains.
- Inside a box, select words and press Alt+Shift+P. The question opens in a box inside, and the
  model also gets the answers around it.
- The answer is saved with the question, and a click on the underline shows both.
