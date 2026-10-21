# Changelog

## 0.5.2 (2026-10-20)

- Peeks (Alt+P) use your chat's model by default. The existing model settings still override it.
- Shorter README, with one image for Codex, Copilot CLI and Kiro CLI. The Claude Code GIF stays.

## 0.5.1 (2026-10-16)

- Fixed: with a long answer open and a long answer in a box inside it, the mouse wheel often
  scrolled the text inside one of the boxes, not the chat. Now text never scrolls inside a box,
  in every agent and on every screen. Each box is as tall as its answer, taller than the screen
  when it has to be. The wheel and PgUp/PgDn scroll the chat with the boxes in it, the same as
  without peekme.
- On Claude Code's classic screen and Kiro's inline screen peekme now scrolls the chat with the
  box in it itself: 3 rows a turn of the wheel, a page for PgUp and PgDn, from the oldest line
  it saw in your terminal's history to the end of the chat. Before, the wheel gave the mouse
  back to the terminal, which scrolled its history and left the box where it was.
- Over a full screen that leaves the mouse to the terminal (Codex inside tmux with its mouse
  off), the wheel shows the rest of a long box.
- A new box inside a box opens under its line without moving the screen, unless the screen has
  to move up a little to show the new box whole.

## 0.5.0 (2026-10-15)

Everything from the 0.4.6 betas below, now in the normal install (`npm i -g peekme`, Homebrew,
`cargo install peekme`).

- The mouse wheel scrolls the chat while a box is open, in Codex, Claude Code, Copilot CLI and
  Kiro CLI full screen. The chat and the box move together, as far and as fast as the agent
  scrolls without peekme, from the start of the chat to its end. PgUp and PgDn page the chat
  as the agent does. Over Claude Code classic and Kiro inline the wheel scrolls your terminal's
  history, and the box stays open.
- The box starts small and grows with the answer, up to the whole screen. The chat moves away to
  make room and comes back when the box closes. Boxes inside boxes grow the same way.
- The ask key is now Alt+Q (Option+Q on a Mac). Alt+Shift+P switches the keyboard layout on
  Windows and many Linux desktops. It still works.
- Saved answers are kept when you quit the agent. Open the chat again and the dotted underlines
  are back, and a click opens the answer with no new request. They are in
  `~/.local/state/peekme/answers/`, for each agent and directory.
- The boxes take each agent's own colour: orange in Claude Code, cyan in Codex, and purple in
  Copilot CLI and Kiro CLI. `PEEKME_COLOR` sets one colour for all agents, and
  `PEEKME_CLAUDE_COLOR`, `PEEKME_CODEX_COLOR`, `PEEKME_COPILOT_COLOR` or `PEEKME_KIRO_COLOR` one
  for an agent. A value is a hex colour (`#d77757`), a terminal colour name (`cyan`,
  `bright-magenta`) or a 256-colour number (`173`).
- Copilot CLI 1.0.98's new input box and Codex 0.162's prompt row at the top are known, so
  neither scrolls with the chat or ends up under the box.
- Fixed on Kiro inline and Claude Code classic: text selected up in the terminal's history, and
  words selected in a box after a turn of the wheel, opened "The selected text isn't on screen".
- New README GIFs and website clips for all four agents, starting on a chat already on the
  screen. The crate no longer carries the GIFs and the website.

## 0.4.6-beta.19 (2026-10-15)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.19`.

- Fixed: saved answers were lost when you quit the agent. Open the chat again (`claude
  --resume`, `codex resume`, `copilot --resume`, `kiro-cli chat --resume`) and the dotted
  underlines are back, and a click on one opens its answer and the boxes inside it with no new
  request. peekme keeps them in `~/.local/state/peekme/answers/`, for each agent and directory.
  An underline comes back only where the words stand amid the same text as before, so the same
  words in another chat don't open an old answer.
- Fixed: on Kiro CLI's inline screen, and Claude Code's classic one, a peek inside a box failed
  after you had turned the mouse wheel. The wheel hands the mouse back to the terminal so it can
  scroll its history, so selecting words in the box made a terminal selection, and Alt+P showed
  "The selected text isn't on screen" and closed the box. Now Alt+P on words selected in a box
  opens a box inside it, whoever made the selection.

## 0.4.6-beta.18 (2026-10-15)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.18`.

- Fixed: in Kiro CLI's inline screen, and Claude Code's classic one, text selected up in the
  terminal's history opened "The selected text isn't on screen". A long answer scrolls off the
  top, and pressing Alt+P scrolls the terminal back down, so peekme looked for the text on a
  screen that no longer showed it. Now it also looks in the last 2000 lines it saw, and the box
  opens at the top of the screen.

## 0.4.6-beta.17 (2026-10-14)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.17`.

- Scrolling Kiro CLI's full screen with a peek box open now works like in Codex, Claude Code and
  Copilot CLI: the chat and the box move together, as far as Kiro scrolls without peekme. That is
  2 rows a turn of the wheel, or what Kiro's Scroll speed setting says, and a screen less 2 rows
  for PgUp and PgDn.
- Fixed: the box closed at the first turn of the wheel over Kiro's full screen. While scrolled up,
  Kiro writes "2 more lines (click or End)" over the line above its input area, and peekme no
  longer recognized that line, so it thought Kiro's input area had moved.
- Fixed: a box opened while Kiro was scrolled up could cover Kiro's input area.
- Fixed: Kiro's "Copied" notice above its input area showed up as a line of the chat next to the
  box.
- End, and Home with nothing typed, make Kiro jump to the end or the start of the chat. With a box
  open this now closes the box.

## 0.4.6-beta.16 (2026-10-14)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.16`.

- Scrolling GitHub Copilot CLI with a peek box open now works like in Codex and Claude Code: the
  chat and the box move together, as far as Copilot scrolls without peekme. That is 3 rows a turn
  of the wheel, 1 with Alt held, and a whole screen for PgUp and PgDn.
- Fixed: Copilot's tabs, its status line and, since Copilot CLI 1.0.98, its input box scrolled
  along with the chat, and the box could cover them. Copilot 1.0.98 draws its input box in a new
  way that peekme did not know.
- Fixed: a quick turn of the wheel or a trackpad flick made the chat jump with a box open.
  Copilot draws turns that arrive together as one jump, sometimes further than a screen, and
  peekme lost track of the box. Now peekme hands Copilot the turns a few at a time, and one at a
  time where many lines read the same.
- Copilot's scrollbar stays as Copilot draws it instead of moving with the lines.
- Fixed: a selection over two lines in Copilot opened "The selected text isn't on screen".
  Copilot copies the text without its scrollbar, and peekme looked for it with the scrollbar.
- Home or End with nothing typed makes Copilot jump to the start or the end of the chat. With a
  box open this now closes the box, as Ctrl+Home and Ctrl+End do.

## 0.4.6-beta.15 (2026-10-14)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.15`.

- Scrolling Claude Code's full screen with a peek box open now works like in Codex: the chat and
  the box move together, as far as Claude scrolls without peekme.
- Fixed: the chat jumped and moved too fast with a box open. Claude speeds up the wheel when you
  turn it quickly and does not move on the first turn the other way. peekme expected a fixed
  step and filled the difference with rows from the box. Now the box moves exactly as far as
  Claude's chat moved, and the box's own rows show only at the top or the bottom of the chat.
- Fixed: Claude's "Jump to bottom" hint, the prompt it keeps at the top and its "thinking" line
  could show up as lines of the chat next to the box.
- Fixed: the box could cover Claude's notice line above its input box.
- PgUp and PgDn with a box open over Claude page the chat as Claude does, half a screen.
- While Claude scrolls fast it leaves some lines blank for a moment. With a box open, peekme
  shows the text it has already seen there.
- A very fast scroll can move Claude's chat more than a screen at once. If peekme can't tell where
  the box went, it shows Claude's screen as it is until the box's lines come back into view.

## 0.4.6-beta.14 (2026-10-13)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.14`.

- A box opened inside another box (Alt+Q or Alt+P on words in a box) now starts as small as its
  text and grows as the answer comes in, like the outer box. Before, it opened at the most rows
  it could take, so a question box was much taller than its two lines.

## 0.4.6-beta.13 (2026-10-13)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.13`.

- Fixed: with a peek box open, the last line of the chat, just above "Back to bottom", showed
  whatever Codex had there without the box: a line from elsewhere in the chat, a blank row, or
  the selected line in place of the box's border. In a terminal that tells programs its colours
  (GNOME Terminal, iTerm2, Terminal.app), Codex tints its input area and pads it with a row above
  the prompt. peekme took that row for Codex's status line and thought the chat ended a row
  higher. Now it counts from the prompt line.

## 0.4.6-beta.12 (2026-10-13)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.12`.

- Fixed: with a peek box open, the last line of the chat above Codex's input sometimes showed
  and sometimes didn't as you scrolled. Codex 0.162 shows the prompt you are reading the answer
  to on the chat's top row, and that row comes and goes at every prompt. When it came, one turn
  of the wheel moved no chat row. peekme took that for the end of the chat and moved the box a
  row on its own. Now peekme knows that row, and the box moves exactly as the chat does.
- Fixed: the prompt from that top row could show up as a line of the chat next to the box.
- Fixed: PgUp and PgDn with a box open did nothing while Codex showed that row.

## 0.4.6-beta.11 (2026-10-13)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.11`.

- With a peek box open the chat scrolled faster than without it. Codex 0.162 (Codex updates
  itself) scrolls 1 row per turn of the wheel by default, not 3. peekme took those short moves
  for the end of the chat and added rows from the box to each one. Now peekme counts rows the
  way Codex does, with Codex's `tui.mouse_scroll_speed` setting, so the chat moves at the same
  speed with a box open.
- peekme now finds the end of the chat by comparing the rows Codex moved with the rows it
  should have moved, not by how fast Codex answers. A slower machine can't make the box jump.

## 0.4.6-beta.10 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.10`.

- Scrolling Codex's chat with a peek box open now works like scrolling a page with the box
  printed in it: every turn of the wheel moves the chat and the box 3 rows, from the start of
  the chat to its end. This was checked frame by frame against real Codex.
- Fixed: after the box went off the screen, the chat moved 6 rows per turn for a while.
- Fixed: the box jumped when its answer was still coming in while you scrolled.
- Fixed: at the start and the end of the chat a turn could move only 1 or 2 rows.
- Fixed: many rows with the same text could make the chat skip.
- Fixed: while Codex works, its "Working" line was taken for chat. A blank row could show next
  to the box. Now the box stays whole above that line.
- PgUp and PgDn with a box open scroll the chat by a page, as in Codex. A box with more text
  than the window still scrolls its own text first. Ctrl+Home and Ctrl+End close the box and
  jump to the start or the end of the chat.

## 0.4.6-beta.9 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.9`.

- The ask key is now Alt+Q (Option+Q on a Mac), Q for question. Alt+Shift+P switches the
  keyboard layout on Windows and on many Linux desktops. Alt+Shift+P still works. On a Mac,
  Option+Q types `œ`: like `π`, it is the key only when text is selected or a box is open, and
  it stays a letter once you type French.

## 0.4.6-beta.8 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.8`.

- Scrolling with a trackpad while a peek box was open made the box slide away from its text,
  faster than the chat. A trackpad also sends small sideways scrolls, and peekme took them for
  up and down. Codex does not scroll for them, so peekme thought the chat was at its end and
  scrolled on by itself. Now sideways scrolls are ignored while a box is open.

## 0.4.6-beta.7 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.7`.

- Scrolling down to the end of the chat with a peek box open stopped short first, and then the
  last rows jumped in a moment later, all at once. peekme waited until the wheel had been still
  for a while before it scrolled the rest. Now it sees that Codex is at the end from Codex's own
  answer to each turn of the wheel, so the last rows scroll in with the wheel, a few rows at a
  time, also when you scroll fast.

## 0.4.6-beta.6 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.6`.

- With a peek box open you could not scroll all the way to the end of the chat: the last rows,
  as many as the box is tall, stayed below the screen. The box makes the chat longer than Codex
  knows, so Codex stops scrolling too early. Now when Codex does not move for the wheel, peekme
  scrolls the rest itself, and the box moves up with its text. The same at the start of the
  chat.

## 0.4.6-beta.5 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.5`.

- Scrolling with a peek box open could leave a blank band where the box had been, with stray
  bits of the chat in it. peekme took Codex's status line and the empty row under it for chat,
  and they don't scroll. Now they stay with Codex's input area and stay live, so a status like
  "Copy sent to terminal" shows there as usual.
- When the box has scrolled off the screen, the chat rows it pushed away come back as you keep
  scrolling, so the screen is soon exactly Codex's again.

## 0.4.6-beta.4 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.4`.

- Scrolling the chat with a peek box open no longer flickers or lags. Codex's frames used to
  reach the terminal first and peekme then drew the box again on top. Now peekme draws each
  frame itself, the chat with the box in it, and writes only the rows that changed, in place.
  Closing the box after a scroll works the same way.
- The box stays right under the selected text (or right above it) while the chat scrolls,
  also when that text goes off the screen. Then the box is cut at the edge.

## 0.4.6-beta.3 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.3`.

- The mouse wheel scrolls the chat again while a peek box is open. Codex and other full screen
  agents get the wheel and scroll as usual, and the box moves with the selected text. With Claude
  Code classic or Kiro inline the first turn of the wheel gives the mouse back to your terminal,
  so the next turns scroll its history. The box stays open in both cases. Only a box with more
  text than the screen has rows still takes the wheel.
- A question asked with Option+Shift+P: when the model wrote a sentence, then thought or used a
  tool, the rest came right after that sentence with no space. Now there is an empty line
  between them.

## 0.4.6-beta.2 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.2`.

- The peek box no longer stops growing at the edge of its side of the selection. When the
  answer needs more rows, the chat text moves away to make room, the selection too, and comes
  back when the box closes. Now the box shows the whole answer. It scrolls only when the
  answer is taller than the screen.

## 0.4.6-beta.1 (2026-10-12)

A test build, only on crates.io: `cargo install peekme --version 0.4.6-beta.1`.

- The peek box now starts small, one line while the model thinks, and grows with the answer
  as it comes in. It can take all the rows on its side of the selection, so a normal answer
  fits without scrolling. When the answer is longer than that, the box scrolls like before.

## 0.4.5 (2026-10-11)

- Mac Terminal: with Kiro CLI inline or Claude Code's classic screen, Option+P typed `π`,
  because Terminal keeps its selection to itself. Now peekme can read that selection through
  macOS Accessibility. The first Option+P with nothing selected that peekme can see asks once:
  press Enter, then turn on Terminal in System Settings > Privacy & Security > Accessibility.
  Esc keeps things as before. Kiro stays inline. `peekme doctor` says if it is on.
- tmux with `mouse on`: Option+Shift+P (Alt+Shift+P) did nothing after a drag when tmux stayed
  in copy mode. Option+P worked. Now both work.
- tmux with `mouse on`: a click on underlined text now opens the saved answer also over agents
  that leave the mouse alone (Claude Code classic, Kiro CLI inline), also over SSH. tmux keeps
  plain clicks for itself there, so peekme asks it to hand them over in its own pane. Drags and
  the wheel stay with tmux. Needs tmux 3.2 or newer.
- Docs: a new table in the README and on peekme.dev says what peekme can read inside tmux and
  over SSH, for each agent, with tmux's mouse on and off.

## 0.4.4-beta.2 (2026-10-11)

A test build, only on crates.io: `cargo install peekme --version 0.4.4-beta.2`.

- tmux with `mouse on`: after a drag that tmux handles (the agent leaves the mouse alone, like
  Kiro inline or Claude classic), Option+Shift+P did nothing when tmux stayed in copy mode.
  Option+P worked. Now both work, also as Alt+Shift+P.
- tmux with `mouse on`: a click on underlined text did nothing when the agent leaves the mouse
  alone, because tmux keeps plain clicks. peekme now asks tmux to hand them over in its own
  pane, so the click opens the saved answer. Drags and the wheel stay with tmux. Needs tmux 3.2
  or newer.

## 0.4.4-beta.1 (2026-10-11)

A test build, only on crates.io: `cargo install peekme --version 0.4.4-beta.1`.

- Mac Terminal: Kiro inline (and Claude classic) leave the mouse to Terminal, and Terminal keeps
  the selection to itself, so Option+P typed π. Now peekme can read that selection through
  macOS Accessibility. The first Option+P with nothing peekme can see asks once: press Enter,
  then turn on Terminal in System Settings > Privacy & Security > Accessibility. Esc keeps
  things as before. Kiro stays inline.
- `peekme doctor` on a Mac says if Accessibility is on for the terminal and, with text selected,
  if peekme can read it.

## 0.4.3 (2026-10-09)

- Questions inside boxes. Select words in a box and press Alt+Shift+P (Option+Shift+P on a Mac):
  a box inside opens for your question, and the answer knows the boxes around it. With boxes
  open and nothing selected, Alt+Shift+P asks about the words of the innermost box.
- Fixed: after a peek inside a box, a new answer for the same text (Alt+P again, or a question)
  showed the old dotted lines on the new text until it finished.
- Fixed: a question typed fast and sent with Enter in the same keystroke batch was not sent.
- Fixed: a saved answer to a question, opened again, had its dotted lines in the wrong place.

## 0.4.2 (2026-10-09)

- Ask a question about the selection: Alt+Shift+P (Option+Shift+P on a Mac) opens the box empty,
  you type the question and press Enter. The answer comes from the model your chat runs on, with
  the whole conversation. Set `PEEKME_CLAUDE_ASK_MODEL`, `PEEKME_CODEX_ASK_MODEL`,
  `PEEKME_COPILOT_ASK_MODEL` or `PEEKME_KIRO_ASK_MODEL` for another model. With a box open,
  Alt+Shift+P asks about the text that box explains.
- Kiro CLI support. `peekme install` now also sets up `kiro-cli`, and a setup from an older
  peekme gets it the next time peekme starts. Everything works as in the other agents: select
  text, press Alt+P (Option+P on a Mac), peek inside a peek, click underlined text for the saved
  answer, also inside tmux and over SSH.
- Kiro starts inline and leaves the mouse to the terminal. After `/fullscreen` Kiro makes the
  selection itself and peekme reads it, also over SSH. Underlined text can be clicked only there.
- Explanations go through `kiro-cli acp` with your Kiro login, on `claude-haiku-4.5` without
  tools. About 0.01 Kiro credits each. The sessions are deleted afterwards, so they don't show up
  in Kiro's session list. Set `PEEKME_KIRO_MODEL` for another model.
- The model knows the Kiro conversation, read from `~/.kiro/sessions/cli`.

## 0.4.1 (2026-10-07)

- Mac: after `peekme install`, `claude` could stop with `error: unknown option
  '--internal-jobctl-helper'` and Claude Code never opened. `codex` and `copilot` had the same
  problem. On macOS peekme's helper process started under the name of the agent link, and
  peekme passed its own flag on to the agent. Now the helper is found first. The same happened
  on any system where `claude` was a copy of peekme instead of a link.
- Tests no longer depend on running inside tmux or over SSH.

## 0.4.0 (2026-10-06)

Everything from the 0.4.0 betas below, now in the normal install (`npm i -g peekme`, Homebrew,
`cargo install peekme`).

- Text you already asked about gets a dotted underline. Click it and the saved answer opens
  again, with no new request. Click again to close it.
- Peek inside a peek. Select words in the box and press Alt+P (Option+P on a Mac). A new box
  opens under that line, and you can go several levels deep.
- Each box scrolls on its own: the wheel scrolls the box under the pointer, PgUp/PgDn the
  innermost one, Esc closes the innermost one.
- Underlines stay on their text after scrolling, resizing and unfolding tool calls in Codex and
  Claude Code.
- Mac fixes: Option+P after a drag in Codex, Terminal with mouse reporting off, and Option sent
  as Alt.

## 0.4.0-beta.10 (2026-10-04)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.10`.

- Clicks on underlined text answer at once. Before, a click soon after the last one was taken as
  part of a double click and did nothing, so you could not close a peek and open it again quickly.
  Now only a second click on the same spot right after opening counts as a double click, and it
  keeps the box open.
- With a box open, a click on other underlined text opens its answer in one click.
- Inside a box, a double click on a word selects it, so Option+P can explain it without a drag.
- Underlines no longer blink after a box closes.

## 0.4.0-beta.9 (2026-10-04)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.9`.

- Peek inside a peek. Select words in the box with the mouse and press Option+P (Alt+P): a new
  box opens inside it, under that line. The outer boxes stay around it, so you see where you are.
  You can go several levels deep while the window is wide enough.
- Each box scrolls on its own. The wheel scrolls the box under the pointer, PgUp/PgDn the
  innermost one. Esc closes the innermost box.
- Answers inside a box are saved like the ones on screen. Their words get the dotted underline,
  and a click on them shows the saved answer again with no new call.
- A press inside the box now selects text there. Before, it closed the box.
- With an agent that does not use the mouse (Claude Code classic, for example), peekme turns
  mouse reports on while a box is open, so text in it can be selected. The agent gets none of them.

## 0.4.0-beta.8 (2026-10-03)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.8`.

- Codex: the underline under explained text lagged when you unfolded a command or a reasoning
  block. The text moved down at once, but the underline came after Codex went quiet, and for that
  time it stayed on the old place. Now peekme draws it in the same frame as Codex's text.

## 0.4.0-beta.7 (2026-10-02)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.7`.

- Codex: Option+P pressed right after a drag sometimes typed the letter instead of opening the
  box. Codex draws its selection a moment after you release the mouse, and peekme looked too
  early. Now it waits up to a second for the selection.

## 0.4.0-beta.6 (2026-09-30)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.6`.

- Mac Terminal with View > Allow Mouse Reporting off: a drag is Terminal's own selection, which
  Codex and peekme never get, so Option+P typed the letter. Now Option+P tells you once to turn
  mouse reporting on (Cmd+R). Alt+P says it every time.
- Option+P is also understood when the terminal reports Option as Alt.

## 0.4.0-beta.5 (2026-09-30)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.5`.

- Mac, without tmux or SSH: Option+P with nothing selected explained the last thing Codex copied
  to the clipboard. Now a copy counts once. After that Option+P types the letter again, until you
  select or copy something new.

## 0.4.0-beta.4 (2026-09-30)

A test build, only on crates.io: `cargo install peekme --version 0.4.0-beta.4`.

- Underlined text that is only partly on screen keeps the line on the part you can see.
- After you resize the window, underlines find their text again where the lines wrap now.
- Leftover underlines no longer stay behind on other text after scrolling in Codex.
- Codex: a selection over several formatted lines now gets its underline too.

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
