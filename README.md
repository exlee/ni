# ni

A small **terminal label writer**, made mostly for [RACE](https://race-term.com).

`ni` is a tiny vim-based modal editor that keeps your text centered on
screen — either each line individually or the whole block at once — so what you
type reads as a label, not a document. It auto-saves whenever a file path is
known.

Press `B` in normal mode at any time to switch between the two centering
styles: **line** centering (every line centered on its own) and **block**
centering (the text keeps its shape and the whole block is centered as one).

## Usage

```sh
ni [--block] [file]
```

- `--block` — center the whole text block instead of each line (toggle at
  runtime with `B` in normal mode).
- With a `file` argument the buffer is auto-saved as you type. Without one,
  press `Ctrl+S` to pick a path.

**`Ctrl+C` to quit, `Ctrl+S` to save** — these work from anywhere, no vim
knowledge required.

## Keys

`ni` starts in normal mode with the usual vim vocabulary:

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` / arrows | move |
| `w` / `b` | word forward / back |
| `0` `^` `$` | line start / first non-blank / line end |
| `gg` / `G` | first / last line |
| `i` `a` `I` `A` `o` `O` | enter insert mode |
| `x` / `D` / `dd` / `dw` | delete char / to end of line / line / word |
| `u` | undo |
| `B` | toggle block/line centering |
| `Ctrl+S` | save as (prompt for path) |
| `ZZ` | save and quit |
| `ZQ` / `Ctrl+C` | quit without saving |

In insert mode: `Esc` back to normal, `Tab` inserts four spaces, arrows move.

## Install

```sh
cargo install --git https://github.com/exlee/ni
```

Or clone and `cargo build --release`; the binary is `ni`.

## License

[MIT](LICENSE)
