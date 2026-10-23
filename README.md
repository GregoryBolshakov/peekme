# peekme

Select text in [Claude Code](https://github.com/anthropics/claude-code),
[Codex CLI](https://github.com/openai/codex),
[GitHub Copilot CLI](https://github.com/github/copilot-cli) or [Kiro CLI](https://kiro.dev/cli/)
output, press **Alt+P**, and an explanation opens right under it, inside the terminal.
The other lines move to make room, like Peek in VS Code. **Esc** closes the box.
On a Mac, use **Option+P**.

![Claude Code: explain selected text, ask a question inside the explanation, and scroll with both boxes open](https://raw.githubusercontent.com/GregoryBolshakov/peekme/main/docs/demo-claude.gif)

## What it does

- **Peek.** Get a short explanation using your chat's model. It gets context from your chat,
  so "its split panes" in an answer about Kitty gets explained as Kitty's split panes.
- **Ask.** Select text and press **Alt+Q** to type your own question, such as "why not use a
  mutex here?". It goes to your chat's model with the whole conversation.
- **Boxes inside boxes.** Select words in an explanation and press Alt+P or Alt+Q again.
  A new box opens inside it, under that line.
- **Saved answers.** Explained text gets a dotted underline. Click it to reopen the answer
  without another request. Saved answers come back when you reopen the chat later.

![Codex CLI with a nested question, GitHub Copilot CLI and Kiro CLI with one explanation each](https://raw.githubusercontent.com/GregoryBolshakov/peekme/main/docs/demo-agents.png)

## Install

You need one of the supported agents installed and logged in. Then run:

```sh
npm i -g peekme
peekme install
```

Open a new terminal. Start `claude`, `codex`, `copilot` or `kiro-cli` as always.
peekme opens with the agent, including when you resume an existing chat.

You can replace the npm command with one of these, then run `peekme install`:

- Homebrew: `brew install gregorybolshakov/tap/peekme`
- Cargo: `cargo install peekme`
- Shell installer: `curl -LsSf https://github.com/GregoryBolshakov/peekme/releases/latest/download/peekme-installer.sh | sh`

Binaries for Linux and macOS, x86_64 and ARM, are on the
[releases page](https://github.com/GregoryBolshakov/peekme/releases/latest).

## Use

Select text in your agent's output with the mouse. On a Mac, use Option in place of Alt.

| Key | What it does |
|---|---|
| Alt+P | Explain the selection |
| Alt+Q | Type a question about the selection, then Enter to send |
| Alt+P again | Explain again using the whole conversation |
| Click underlined text | Reopen a saved answer. Click again to close it |
| Mouse wheel or PgUp / PgDn | Scroll the chat with the boxes in it |
| Esc | Close the innermost box, or cancel a question you are typing |

With a box open, select words inside it to open a nested peek or question. You can keep typing
in the agent's input line while an explanation is open. Enter sends your message and closes
the boxes.

In Claude Code, peekme uses Alt+P for peeking. Use `/model` to open Claude's model picker.
See [all keys and gestures](https://peekme.dev/docs/keys/).

## Models and cost

peekme uses the agent's existing login. There is no separate account or API key.
Each explanation uses a little of your agent's plan or credits.

Peek and Ask use your chat's model by default. Peek sends relevant parts of the conversation.
Ask sends the whole conversation. You can change either model in
[settings](https://peekme.dev/docs/settings/).

Text goes through your agent's own CLI to its provider. The temporary explanation sessions
are not kept in your agent's chat history. Read [what gets sent](https://peekme.dev/privacy/)
and [how peekme works](https://peekme.dev/how-it-works/).

## Help

For Terminal and iTerm2 setup, see the [Mac guide](https://peekme.dev/docs/macos/).
Over SSH, install peekme on the remote machine where your agent runs. If you use Codex inside
tmux, turn its mouse on with `set -g mouse on` and restart Codex.
See the [tmux and SSH guide](https://peekme.dev/docs/tmux-ssh/).

If something does not work, start with:

```sh
peekme doctor
```

See [troubleshooting](https://peekme.dev/docs/troubleshooting/) for selection problems and known
limits. To remove peekme, run `peekme uninstall`. To run an agent without it once, use
`command claude` (or `codex`, `copilot`, `kiro-cli`).

Full documentation: [peekme.dev/docs](https://peekme.dev/docs/).

## License

MIT or Apache-2.0, at your choice.
