# Job control

## What `kill(0, SIGTSTP)` does

### In short

It sends **SIGTSTP** to every process in the *caller's* process group, so the shell and its children stop together. Use ~~SIGSTOP~~ only when the group must not catch it; see [the man page](https://man7.org/linux/man-pages/man2/kill.2.html) or <https://example.com/signals>.
=====
Three things happen:
- the kernel marks each process in the group as **stopped**, which takes a moment on a busy machine with many threads
- the parent gets `SIGCHLD`
  - with `WUNTRACED`, `waitpid` reports it
    - deeper note
- the terminal is left alone

1. first step
2. second step
   1. nested step
      1. third level
3. third step

- [ ] open task
- [x] done task
=====
Para one

Para two
line two right after a single newline
- x
- y

End of it.

- a

- b
- c
After loose.
=====
> A quote with **bold** inside that is long enough to wrap around the width of this box.
>
> Second paragraph of the quote.
>> Nested quote line.
> - list in quote
> - second item

After the quote.
=====
| Call | Effect | Code |
|:-----|:------:|-----:|
| `kill(0, sig)` | **whole group** | 0 |
| `kill(pid, sig)` | one process | -1 |

Text after the table.
=====
| Name | What it does |
|---|---|
| peekme | a wrapper that explains selected text in agent chats in a small box under it |
| codex | the OpenAI agent CLI that runs in the terminal and edits code |
=====
| A | B | C | D | E |
|---|---|---|---|---|
| alpha beta gamma | delta epsilon zeta theta iota kappa lambda mu nu xi omicron | eta theta iota | kappa lambda mu | nu xi omicron pi |
| one | two | three | four | five |
=====
Run this:

```
cargo build --release
    indented line inside the fence
a very long line of code that is longer than the width of the box for sure and then some more
```

    indented code block
end
=====
Before the rule.

---

## After the rule
Text right after a heading.
=====
A paragraph with a very-long-unbreakable-word-that-is-longer-than-the-width-of-the-terminal-for-sure and more words after it to wrap, plus  two  spaces.
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx next word
=====
Escaped \*not italic\* and snake_case_word and 2*3*4 and a **bold `code` inside** and *italic `code`* and ~5 s to ~10 s.
=====
- item with a table:

  | k | v |
  |---|---|
  | a | b |
- item with code:
  ```
  code in item
  ```
- item with a quote:
  > quoted in item
- last
=====
| Only | Header |
|---|---|
=====
The box shows an *explanation* of the selected text. It uses a **smaller model** so the answer comes back fast:

1. peekme reads the selection and the chat around it
2. it asks the model for a short explanation
3. the answer streams into the box

Keys: `Alt+P` opens it, `Esc` closes it.
