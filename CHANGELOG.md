# Changelog

## 0.1.3 (2026-09-27)

- peekme uses the shared Codex server that Codex 0.157 runs in the background, instead of
  starting its own. A session now costs a few MB instead of about 250 MB. The connection is
  made on the first Alt+P and made again if it drops. Without the shared server, peekme starts
  its own server as before. `PEEKME_OWN_SERVER=1` forces that.
- Alt+P again on a long chat shows the size first. Above about 40k tokens it asks for one more
  Alt+P before it sends the whole conversation.

## 0.1.2 (2026-09-27)

- Works with Codex 0.157, which draws full screen and handles the mouse itself. peekme uses the
  text Codex highlights when you drag. Moving the mouse no longer closes the box, the mouse
  wheel scrolls it, and a click closes it and still reaches Codex.
- The Codex input line stays live under the box in full screen too, and the box closes by
  itself when Codex asks for approval.

## 0.1.1 (2026-09-27)

- Text in the box is cleaned before it is drawn. The explanation, the selected text and error
  messages can no longer send terminal control sequences, for example one that writes your
  clipboard, or characters that reverse the order of the text.

## 0.1.0 (2026-09-26)

- `peekme install` makes `codex` open with peekme. It adds a shell function `codex` and a
  `codex` link at the front of PATH. Aliases and scripts that call `codex` keep working.
  `peekme uninstall` removes it and leaves your files as they were. `peekme doctor` checks the
  setup and says what is in the way.
- The first plain `peekme` run asks once if it should set this up.
- Only Codex's interactive screen gets peekme. `codex exec`, `login`, `mcp` and the other
  subcommands, and output to a pipe or a file, run plain Codex directly.
- peekme never wraps itself twice.
- Ctrl+Z works. Codex stops, you get your shell back, and `fg` brings it back. Before, Ctrl+Z
  did nothing inside peekme.
- The Codex input line and status line stay live while the box is open. You can type, and
  Enter sends the message and closes the box. If Codex needs more room, for example for an
  approval question, the box closes by itself.
- If peekme can't start its terminal, it starts Codex directly. If something inside peekme
  fails later, it logs the error, puts the screen back and keeps passing Codex through.
  The log is in `~/.local/state/peekme/peekme.log`.
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
