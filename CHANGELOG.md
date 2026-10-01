# Changelog

## 0.4.0-beta.3 (2026-09-30)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.3`.

- In Codex and Claude Code full screen, an underline could disappear after a small scroll or after
  opening a tool call, when the same words were on screen twice (for example in your question and
  in the answer). peekme moved the mark to the other copy and never drew it there. Now a mark
  follows its own copy, found by the lines around it.

## 0.4.0-beta.2 (2026-09-30)

A test build, only on crates.io. `cargo install peekme` still gives 0.3.5. To try it:
`cargo install peekme --version 0.4.0-beta.2`. It has all the fixes from 0.3.5.

- Text you already asked about gets a dotted underline once the answer is complete, so you can
  see what you peeked before. The line comes back when the agent redraws those rows. Terminals
  without dotted lines show another kind of underline.
- Click underlined text to see its answer again, with no new request. Click it again to close
  the box. A double click opens it once. Dragging over the text still selects it, and a click with
  Ctrl, Cmd, Shift or Option still does what your terminal or agent does with it. Links in the
  agent's answer stay links. With the mouse over underlined text the line turns solid, and in
  terminals that support it (kitty, Ghostty, WezTerm) the pointer becomes a hand.
- Clicks work where the agent handles the mouse itself: Codex and Claude Code full screen, also
  in tmux and over SSH. In Claude Code's classic screen, select the text and press Alt+P.
- Select marked text and press Alt+P: the answer from before opens right away. Alt+P again still
  asks with the whole chat.
- Codex: Option+P typed `π` when the selected text was in one of Codex's answers. Codex copies
  such a selection as markdown (`- **Desktop app**` for the words Desktop app), and peekme looked
  for that text on the screen and did not find it. Now peekme uses the text Codex highlights.

## 0.3.5 (2026-09-30)

- Option+P did nothing after a tmux selection when tmux stays in copy mode after the drag (a
  `MouseDragEnd1Pane` binding to `copy-selection`, common in tmux configs). tmux took the key for
  its copy mode. peekme now binds Option+P in tmux's copy mode for its own pane: it leaves copy
  mode and explains what you selected. Other panes are not affected, and a key you bound
  yourself is left as it is.
- Over SSH, Alt+P with nothing peekme can see now says that text selected with Option held stays
  in your terminal. iTerm2 and Terminal don't give it to programs, so peekme can't read it.

## 0.3.4 (2026-09-29)

- Codex inside tmux over SSH: Option+P typed `π` although text was selected. Codex (0.159)
  asks tmux whether it handles the mouse, and when tmux has the mouse off, which is its default,
  Codex leaves the mouse to the terminal. The selection is then made by your terminal on your
  own machine, and peekme on the other side of SSH cannot see it. peekme can't change that, so
  now the first Option+P there opens a box that says so and how to fix it: `set -g mouse on` in
  `~/.tmux.conf`. Alt+P shows the same box. Found by driving the real Codex in Terminal and
  iTerm2 on macOS, through SSH and tmux.
- On a Mac reached over SSH, peekme no longer reads that Mac's clipboard as a selection. It is
  not the clipboard of the terminal you type in.
- Short messages in a box now show whole, not one line with PgUp/PgDn.
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
