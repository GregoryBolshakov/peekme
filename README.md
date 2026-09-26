# peekme

Peek me. Pick me. Select text in [Codex CLI](https://github.com/openai/codex) output, press **Alt+P**, and a short
explanation opens right under that text, inside the terminal. The lines below move down to make
room, like Peek in VS Code. Esc closes it and the screen is exactly as before.

The explanation comes from a small, fast model and it knows the conversation. If you select
"its split panes" in an answer that recommends Kitty, it tells you it is about Kitty.

Status: 0.0.1, first working version. Tested on Linux with X11.

## Install

You need Rust and Codex CLI (logged in).

```
cargo install peekme
```

## Use

```
peekme                       # starts codex inside peekme
peekme codex resume --last   # arguments go to codex
alias codex='peekme codex'   # use it every time
```

Select text with the mouse as you normally do, then press Alt+P.

| Key | When | What it does |
|---|---|---|
| Alt+P | any time | explain the selected text |
| Alt+P again | box is open, same selection | explain again using the whole conversation |
| PgUp / PgDn | box is open | scroll a long explanation |
| Esc | box is open | close the box |
| any other key | box is open | close the box, the key goes to Codex |

No API key is needed. peekme asks Codex's own `app-server` with your existing Codex login.
It uses a temporary thread that is not saved, so nothing appears in your session history. It
does use your Codex plan, a little for each explanation.

## How it works

Codex runs in a pseudo-terminal. Its output goes to your terminal unchanged, and the same bytes
also go to a terminal emulator in memory (`alacritty_terminal`). So peekme always knows what
is on your screen.

On Alt+P it reads your mouse selection (the X11 PRIMARY selection) and finds it on the screen.
Then it redraws only the rows below the selection, with the box between them. Nothing is cleared
and there is no switch to another screen, so there is no flicker. Codex output that arrives
while the box is open is kept aside.

On Esc it draws those rows again from a copy taken when the box opened. After that it sends the
kept output, so the terminal ends in the same state as if the box never existed. Tests check
this cell by cell on real Codex output.

### What the model gets

A few words alone are not enough ("Desktop app", which one?). The whole chat is too much and too
slow. So the model gets small parts, each with a size limit:

1. One line saying this is Codex CLI and in which directory. This is enough for text from Codex
   itself, like the startup tips.
2. The text around the selection, with the selection marked in place like `⟦this⟧`. peekme
   finds it in the conversation by searching for the selection together with the words next to
   it on screen. This way it finds the right place when the same words appear many times. If the
   text is not in the conversation, it uses the screen.
3. Your request that this text answers, and one line for each of your earlier requests.
4. The first places in the conversation where the same words were used before.

Usually this is 1 to 7 KB of text. When it is not enough, press Alt+P again and the model gets
the whole conversation.

## Settings

| Variable | Meaning |
|---|---|
| `PEEKME_MODEL` | model for explanations. Default is the model your account lists as fast |
| `PEEKME_CODEX_BIN` | codex binary to use for explanations. Default is `codex` |
| `PEEKME_SELECTION` | use this text instead of the mouse selection, for testing |

## Limits of 0.0.1

- Only text that is on screen now. Text that scrolled up into the terminal history can't be
  selected yet.
- The mouse selection is read on Linux with X11. On Wayland it works only through XWayland. On
  macOS it reads the clipboard instead, and this is not tested. Over SSH there is no mouse
  selection.
- While the box is open, the Codex input line under it does not update. It updates when you
  close the box.
- Ctrl+Z does not suspend Codex when it runs inside peekme.
- An explanation takes about 3 seconds to arrive.

## Development

```
cargo test
cargo clippy --all-targets
cargo run --example vtdump -- capture.bin 120 40 [offset]
cargo run --example peek_text -- "<screen text>" "<selection>" [--deep]
python3 spikes/flicker_test.py
python3 spikes/explain_probe.py
```

`vtdump` replays a captured byte stream and prints the screen. `peek_text` runs one explanation
without a terminal and prints the exact prompt. `flicker_test.py` shows if your terminal flickers
when a program switches screens. `explain_probe.py` makes one explanation request to
`codex app-server` and prints timings.

## License

MIT or Apache-2.0, at your choice.
