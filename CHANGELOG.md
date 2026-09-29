# Changelog

## Unreleased

- With two copies of peekme on one machine (an old one from `cargo install`, a newer one from npm
  or Homebrew), `codex` could keep starting the old copy. The old copy's `peekme install` had
  linked `codex` to itself, and the new copy only fixed links that were missing. So Codex got
  none of the later fixes, for example Option+P inside tmux over SSH, while `claude` worked. Now
  the newer peekme moves such links to itself when it starts, and says so once.

## 0.3.3 (2026-09-29)

- Claude Code with Amazon Bedrock, Google Vertex or an API gateway: explanations failed with
  "Could not load AWS credentials" or "Not logged in", although Claude Code itself worked. The
  explainer did not read your Claude Code settings, so it missed the `env` block, `awsAuthRefresh`,
  `awsCredentialExport`, `gcpAuthRefresh` and `apiKeyHelper` there. It also dropped every
  `CLAUDE_CODE_*` variable, `CLAUDE_CODE_USE_BEDROCK` included. Now it uses the same login and
  provider as `claude` in that directory. Hooks, plugins and MCP servers from your settings still
  do not run for explanations.
- Claude Code's classic screen: when `claude` started lower on the screen, under earlier shell
  output, the box could land a few rows off and repeat lines from above it. peekme now asks the
  terminal where the cursor is when it starts, and the box never covers the shell lines above
  the agent.
- Agents are listed as Claude Code, Codex, Copilot CLI everywhere. Plain `peekme` now runs the
  first one of them that is installed, in that order. Before, it preferred Codex.
- A shell setup from an older version that differs only in this order is left as it is.

## 0.3.2 (2026-09-28)

- Updating peekme now also updates your shell setup. Before, a setup made with an older version
  only knew `codex`, so after updating, `claude` and `copilot` still started without peekme until
  you ran `peekme install` again. Now peekme adds the missing agents the first time it starts
  and tells you once.
- Inside tmux, a selection you cleared with a click right after making it was still explained by
  the next Alt+P or Option+P. peekme now only takes a copy the agent made after your last mouse
  release.

## 0.3.1 (2026-09-28)

- Alt+P and Option+P work inside tmux with Claude Code and Codex. Inside tmux these agents do not
  send a mouse selection to the terminal, they give it to tmux, so peekme saw nothing and Option+P
  typed `π`. peekme now reads the selection from tmux. This also covers SSH with tmux on the
  other side.
- With tmux handling the mouse (`set -g mouse on`) and Claude Code's classic screen, a selection
  made with tmux can be explained too.
- Every release is now tested through tmux and SSH in many combinations, and in the real Terminal
  and iTerm2 on macOS, in their default settings and with Option as Meta.

## 0.3.0 (2026-09-28)

- peekme works with GitHub Copilot CLI too. Type `copilot` as always, select text, press Alt+P.
  Run `peekme install` once more so that `copilot` gets peekme.
- Copilot explanations come from Copilot CLI's own headless server with your Copilot login.
  Each one is a short session without tools, and peekme deletes it afterwards, so it does not
  show in your session list. The context comes from the running session's log.
- peekme reads Copilot's selection from the OSC 52 copy it sends when you release the mouse,
  also over SSH and in tmux. Copilot's input line stays live under the box.

## 0.2.3 (2026-09-28)

- Option+P works on a Mac also over SSH and inside tmux. 0.2.2 found out from the environment
  that the keyboard is a Mac's, and over SSH or in tmux it could not. Now `π` explains whenever
  something is selected, and is typed as usual when nothing is.
- Alt+P arrives correctly when tmux sends keys in its extended format (`extended-keys`).
- Selecting only empty space no longer opens a box.

## 0.2.2 (2026-09-28)

- Option+P works on a Mac as it is. Mac terminals send Option+P as `π`, and peekme now takes
  that as the shortcut, like Claude Code does. Before, you had to set Option to act as Meta in
  the terminal. Also over SSH from Terminal or iTerm2.
- On macOS peekme finds the running Claude Code session exactly. Before, it took the newest
  session in the directory, which is wrong when two run there.

## 0.2.1 (2026-09-28)

- You don't need Rust any more. peekme can be installed with npm (`npm i -g peekme`), with
  Homebrew (`brew install gregorybolshakov/tap/peekme`) or with a shell script. Each release has
  ready binaries for Linux and macOS, on x86_64 and ARM.
- Ctrl+Z works when peekme was started through another program, like the npm launcher. Before,
  the terminal could wait forever.

## 0.2.0 (2026-09-28)

- peekme works with Claude Code too. Type `claude` as always, select text, press Alt+P.
  `peekme install` now hooks `claude` next to `codex`. If you installed 0.1, run
  `peekme install` once more.
- Claude Code explanations come from `claude -p` on Haiku with your Claude Code login. The
  session is not saved. The context comes from the running session's transcript, and Alt+P
  again sends the whole conversation (after asking, when it is big).
- In Claude Code's full screen mode peekme reads the selection from the OSC 52 copy Claude sends
  when you release the mouse. In the classic mode it reads the X11 PRIMARY selection.
- Claude's input line stays live under the box, as with Codex. Ctrl+Z works with Claude too.
- The box is never drawn in the middle of an escape sequence from the agent. Before, if the
  agent's output stopped inside one (a window title, say), part of it could show as text.
- While an explanation streams in, the box is redrawn at most about 30 times a second. This
  sends much less to the terminal.
- `peekme doctor` checks both agents. Plain `peekme` runs Codex, or Claude Code when only that
  is installed.

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
