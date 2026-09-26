# Changelog

## Unreleased

- The box gets smaller when the explanation is complete, so it takes only the rows it needs.
- A comma or a bracket after styled text no longer starts a new line in the box.
- The terminal cursor is hidden while the box is open. Before, it could show inside the box text.

## 0.0.1 (2026-09-26)

First working version.

- Runs Codex (or any command) in a pseudo-terminal. Output goes to the terminal unchanged and is
  also kept in a terminal emulator in memory.
- Alt+P explains the text selected with the mouse (X11 PRIMARY selection). The box opens under
  or above the text without clearing the screen. Works with the kitty keyboard protocol and with
  any keyboard layout.
- Esc restores the screen from a copy and then sends the Codex output that arrived meanwhile.
- Explanations come from `codex app-server` with a temporary thread on the account's fast model,
  so no API key is needed.
- The model gets the text around the selection with the selection marked, the request it
  answers, a short list of earlier requests and earlier uses of the same words.
- Alt+P again on the same selection explains with the whole conversation.
