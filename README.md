# peekme

Pick me. Select text in [Claude Code](https://github.com/anthropics/claude-code),
[Codex CLI](https://github.com/openai/codex),
[GitHub Copilot CLI](https://github.com/github/copilot-cli) or [Kiro CLI](https://kiro.dev/cli/) output, press **Alt+P** (**Option+P** on a Mac), and a short
explanation opens right next to that text, inside the terminal. The other lines move to make
room, like Peek in VS Code. Esc closes it and the screen is exactly as before.

![peekme in Claude Code: select "planner statistics" in an answer and press Alt+P. In the explanation select "hash join" and press Alt+P again for a box inside the box. Esc closes them, and a click on the underlined words opens the saved answer](https://raw.githubusercontent.com/GregoryBolshakov/peekme/main/docs/demo-claude.gif)

The explanation comes from a small, fast model and it knows the conversation. If you select
"its split panes" in an answer that recommends Kitty, it tells you it is about Kitty.

Status: 0.4.1. Tested with Claude Code 2.1.292, Codex 0.161, Copilot CLI 1.0.93 and Kiro CLI 2.28 on Linux, also
inside tmux and over SSH. On macOS it is tested in Terminal and iTerm2 with a stand-in agent, and
in Terminal with the real Codex, also inside tmux and over SSH.

## One peekme for all your agents

Many developers have more than one agent CLI and switch between them during the day. peekme
works the same way in each of them. You install it once, and the key and the box are the same
everywhere. Claude Code, Codex CLI,
GitHub Copilot CLI and Kiro CLI work today, and more agents are planned. If one agent gets its own way to
explain a selection some day, it will work in that agent only. peekme keeps working in all of
them.

Codex CLI:

![peekme in Codex CLI: select "noisy commits" in the answer, press Alt+P, read the explanation, press Esc](https://raw.githubusercontent.com/GregoryBolshakov/peekme/main/docs/demo.gif)

GitHub Copilot CLI:

![peekme in GitHub Copilot CLI: ask about set -euo pipefail, select "pipefail" in the answer, press Alt+P, read the explanation, press Esc](https://raw.githubusercontent.com/GregoryBolshakov/peekme/main/docs/demo-copilot.gif)

## Install

```
npm i -g peekme
peekme install
```

Open a new terminal. Now `claude`, `codex`, `copilot` and `kiro-cli` open with peekme, and you
type them as always. That's all.

peekme has no account of its own. The explanation uses the login of the agent you are in.

Don't have npm? Use one of these instead of the first line:

- Homebrew: `brew install gregorybolshakov/tap/peekme`
- Rust: `cargo install peekme`
- Neither: `curl -LsSf https://github.com/GregoryBolshakov/peekme/releases/latest/download/peekme-installer.sh | sh`

## Use

Start Claude Code, Codex, Copilot CLI or Kiro CLI as always. Select text with the mouse as you
normally do, then press Alt+P. When the agent draws full screen (Claude Code with
`"tui": "fullscreen"`, Codex since 0.157, Copilot CLI, Kiro CLI after `/fullscreen`), it makes
the selection itself, and peekme uses that selection.

| Key | When | What it does |
|---|---|---|
| Alt+P (Mac: Option+P) | any time | explain the selected text |
| Alt+P again | box is open, same selection | explain again using the whole conversation |
| PgUp / PgDn, mouse wheel | box is open | scroll a long explanation |
| Esc | box is open | close the box |
| typing | box is open | goes to the agent's input line, the box stays open |
| Enter | box is open | sends your message and closes the box |
| Alt+P on text in the box | box is open | opens a new box inside it, under that line |
| click on underlined text | any time | shows the answer from before, with no new request |

Text you already asked about gets a dotted underline, so you can see what you peeked before.
A click on it opens the saved answer again, a second click closes it. Inside a box you can select
words and press Alt+P again, and go several levels deep while the window is wide enough. The
wheel scrolls the box under the pointer, Esc closes the innermost box.

### On a Mac

Option+P works as it is, with no terminal setting, also over SSH and inside tmux. A Mac
terminal sends Option+P as the character `π`. When something is selected, peekme takes `π` as
the shortcut. When nothing is selected, `π` is typed as usual, so you never lose the letter. If
you type Greek, `π` stays a letter. If your terminal sends Option as Meta, Option+P works too.

When the agent draws full screen, peekme gets the selection from the agent. Otherwise (Claude
Code's classic screen, Kiro CLI before `/fullscreen`) it reads the clipboard: iTerm2 copies a
selection there by itself, in Terminal press Cmd+C after you select. Over SSH only the agent's
own selection can be read, so use the full screen mode there (Claude Code with
`"tui": "fullscreen"`, `/fullscreen` in Kiro CLI, Codex and Copilot CLI by default).

Inside tmux, Codex leaves the mouse to the terminal when tmux has the mouse off, which is tmux's
default. A selection is then your terminal's own, and over SSH peekme cannot read it. Add
`set -g mouse on` to `~/.tmux.conf` and restart Codex. peekme tells you this the first time you
press Option+P there.

With tmux's mouse on, a drag over text the agent doesn't take is a tmux selection, and Option+P
explains it, also when your tmux stays in copy mode after the drag. A selection made with Option
held is your terminal's own. Over SSH no program on the other side can read it, so drag without
Option there.

In Claude Code, Alt+P opens the model picker. With peekme it explains the selection instead.
`/model` still opens the model picker.

No API key is needed. peekme asks the agent's own CLI, with your existing login:

- Claude Code: `claude -p` on Haiku, without tools and without saving the session, so nothing
  appears in `/resume`. After the first explanation one `claude -p` waits in the background for
  the next one, so it starts in about half a second. It takes about 210 MB.
- Codex: its `app-server`, on a temporary thread that is not saved. Since Codex 0.157 a shared
  Codex server runs in the background, and peekme uses it, so it adds only a few MB of memory.
  Without that server it starts its own `codex app-server`, which takes about 250 MB.
- Copilot CLI: its headless server (`copilot --headless`, the one the Copilot SDK uses), started
  on the first explanation. Each explanation is a short session without tools, and peekme
  deletes it afterwards, so nothing appears in your session list. The model is Copilot's Auto
  unless you set one. It uses a little of your AI credits for each explanation.
- Kiro CLI: its ACP server (`kiro-cli acp`, the one Kiro's own screen and editors use), started
  on the first explanation. It runs with a peekme agent that has no tools. Each explanation is
  a short session, and peekme deletes it afterwards, so nothing appears in your session list.
  The model is `claude-haiku-4.5` unless you set one (Auto if your plan doesn't have it). One
  explanation costs about 0.01 Kiro credits.

It does use your plan, a little for each explanation. Nothing starts before the first Alt+P.

Only the interactive screen gets peekme: `claude`, `claude "prompt"`, `claude --resume`,
`claude -c`, `codex`, `codex "prompt"`, `codex resume`, `codex fork`, `copilot`,
`copilot -i "prompt"`, `copilot --resume`, and `kiro-cli`, `kiro-cli chat`, `kiro-cli --resume`.
Commands like `claude -p`, `codex exec`, `copilot -p`, `copilot login`,
`kiro-cli chat --no-interactive` or `kiro-cli acp`, and anything with output going to a pipe or
a file, run the agent directly.

## How it works

The agent runs in a pseudo-terminal. Its output goes to your terminal unchanged, and the same
bytes also go to a terminal emulator in memory (`alacritty_terminal`). So peekme always knows
what is on your screen.

On Alt+P it takes the selection and finds it on the screen:

- Claude Code full screen: Claude copies the selection with an OSC 52 escape sequence when you
  release the mouse. peekme reads the text from that sequence as it passes through, and knows
  where the drag ended. This also works over SSH.
- Codex full screen: the text Codex highlighted.
- Copilot CLI, and Kiro CLI after `/fullscreen`: the same OSC 52 sequence as Claude Code.
- Otherwise: your terminal's mouse selection (the X11 PRIMARY selection).

Then it redraws only the rows next to the selection, with the box between them. Nothing is
cleared and there is no switch to another screen, so there is no flicker. The agent's input
line and status line under the box keep updating, so you can type while you read. Other output
that arrives while the box is open is kept aside.

On Esc it draws those rows again from a copy taken when the box opened. After that it sends the
kept output, so the terminal ends in the same state as if the box never existed. Tests check
this cell by cell on real Claude Code, Codex, Copilot CLI and Kiro CLI output.

### What the model gets

A few words alone are not enough ("Desktop app", which one?). The whole chat is too much and too
slow. So the model gets small parts, each with a size limit:

1. One line saying which agent this is and in which directory. This is enough for text from the
   agent itself, like the startup tips.
2. The text around the selection, with the selection marked in place like `⟦this⟧`. peekme
   finds it in the conversation by searching for the selection together with the words next to
   it on screen. This way it finds the right place when the same words appear many times. If the
   text is not in the conversation, it uses the screen.
3. Your request that this text answers, and one line for each of your earlier requests.
4. The first places in the conversation where the same words were used before.

The conversation comes from Claude Code's session transcript in `~/.claude/projects`, from
Codex's app-server, from Copilot CLI's session log in `~/.copilot/session-state`, or from Kiro
CLI's session log in `~/.kiro/sessions/cli`. peekme knows which session, because the agent
registers its process id there.

Usually this is 1 to 7 KB of text. When it is not enough, press Alt+P again and the model gets
the whole conversation. A long chat can be 5 to 30 times bigger than a normal explanation, so
above about 40k tokens peekme shows the size first and sends it only when you press Alt+P once
more.

## Settings

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
| `PEEKME_SELECTION` | use this text instead of the mouse selection, for testing |

### How `claude`, `codex`, `copilot` and `kiro-cli` get peekme

```
peekme doctor      # check the setup, and see what is in the way if something is
peekme uninstall   # remove it, your files are as before
command claude     # run plain Claude Code once (same for codex, copilot, kiro-cli)
```

`peekme install` does two things:

1. It defines shell functions `claude`, `codex`, `copilot` and `kiro-cli` in your shell setup. A function wins over
   anything on PATH, so it keeps working when nvm, mise, Homebrew or an agent's installer
   change PATH later. Aliases that call them go through it too.
2. It puts `claude`, `codex`, `copilot` and `kiro-cli` links to peekme in `~/.local/share/peekme/bin` at the front of
   PATH, so scripts that run them also get peekme. If some tool puts its own directory in front
   of it later, only scripts lose peekme, and `peekme doctor` tells you which directory it is.

Both find the real agent on PATH themselves, so updating the agent changes nothing. If you
remove peekme, the functions fall back to the plain agents.

The block in `~/.bashrc` or `~/.zshrc` (for fish, a file in `~/.config/fish/conf.d`) is short
and marked. It covers all four agents, so an agent you install later gets peekme too. When a new
peekme version knows more agents, it adds them to your setup the next time it starts and says
so once.

npm 12 may say that it skipped an install script of peekme. That is fine: peekme then downloads
its binary the first time it runs. Ready binaries for Linux and macOS (x86_64 and ARM) are on the
[latest release](https://github.com/GregoryBolshakov/peekme/releases/latest) page.

To use peekme with another program, run `peekme -- COMMAND`.

## Limits

- Only text that is on screen now. Text that scrolled up into the terminal history can't be
  selected yet.
- In full screen mode selection works anywhere, also over SSH, because peekme reads the
  selection from the agent. Otherwise peekme needs the terminal's selection, which it reads on
  Linux with X11 only. On Wayland it works through XWayland, and on macOS it reads the
  clipboard (see [On a Mac](#on-a-mac)).
- A click on underlined text works only when the agent takes the mouse (full screen mode). On
  Claude Code's classic screen and Kiro CLI before `/fullscreen` the click goes to your
  terminal.
- Claude Code full screen: if `copyOnSelect` is off, Claude does not report the selection. Then
  only a Shift+drag selection works (read from X11 PRIMARY).
- An explanation takes about 1 to 2 seconds to start arriving.

## Development

```
cargo test
cargo clippy --all-targets
cargo run --example vtdump -- capture.bin 120 40 [offset]
cargo run --example peek_text -- "<screen text>" "<selection>" [--deep] [--claude|--copilot|--kiro] [--pid PID]
python3 spikes/capture.py out.bin 120 40 "<script>" -- peekme claude
python3 spikes/flicker_test.py
python3 spikes/explain_probe.py
python3 spikes/copilot_probe.py info
python3 spikes/kiro_probe.py [MODEL] [PROMPT]
```

`vtdump` replays a captured byte stream and prints the screen. `peek_text` runs one explanation
without a terminal and prints the exact prompt. `capture.py` runs a command in a pseudo-terminal,
sends keys and mouse events from a script and saves everything it prints. `flicker_test.py`
shows if your terminal flickers when a program switches screens. `explain_probe.py` makes one
explanation request to `codex app-server` and prints timings. `copilot_probe.py` does the same
with Copilot CLI's headless server, `kiro_probe.py` with Kiro CLI's ACP server.

Each agent has its own folder in `src/` (`claude/`, `codex/`, `copilot/`, `kiro/`): how its selection is read, where
its conversation comes from, and which model explains. The rest is shared.

## License

MIT or Apache-2.0, at your choice.
