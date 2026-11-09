<h1 align="center">haunt</h1>

<p align="center">
  <i>muxed persistent processes that 'haunt' your terminal</i>
</p>

---

https://github.com/user-attachments/assets/289af5ff-8717-4650-a9d8-f3b24bf01283

`haunt` is a small terminal multiplexer for Ghostty. It runs a main shell and lets you summon configured apps with direct keybinds. Hide an app and it keeps running; bring it back and pick up where you left off.

Each Ghostty tab runs its own haunt process. Apps persist while that process is running, independently of programs such as Neovim in the main shell. `haunt` is not a detachable session manager: exiting the main shell or closing the tab ends the session and shuts down its apps.

`haunt` uses libghostty-vt to parse each pane and render it in the outer terminal, with support for features such as Kitty graphics and progress reporting.

## why?

I live in the terminal, and mostly in neovim. I loved [toggleterm.nvim](https://github.com/akinsho/toggleterm.nvim) and its natural successor, [snacks.nvim](https://github.com/folke/snacks.nvim): being able to jump in and out of a terminal buffer for a specific app or task, without ever leaving my editor, is a great workflow. However, there were a couple problems:

- The apps I launched from Neovim lived and died with it. Closing neovim killed the process.
- Embedded terminals had limited support for features like kitty graphics, progress reporting and other OSC sequences.

I wanted to keep the per-app keybinds, but give each app a full-featured terminal that outlives whatever I'm doing in the main pane. Enter `haunt`.

## usage

Run `haunt` inside Ghostty. Your shell opens as the main pane. On first run, haunt creates `~/.config/haunt/config.toml` with a default configuration. Edit it to add your own apps and bindings, then press `ctrl+shift+r` to reload.

Press a binding to start and show its app; press it again to hide it. Only one app is visible at a time, but hidden apps continue running. Switching to another app hides the previous one. If an app exits, its next invocation starts a fresh process. New apps start in the main pane's current working directory when it can be determined.

A half-size app appears to the right of the main pane; a full-size app covers it. Click a pane to focus it.

## configuration

Create `~/.config/haunt/config.toml` (or `$XDG_CONFIG_HOME/haunt/config.toml` when `XDG_CONFIG_HOME` is absolute):

```toml
# Optional; defaults to your account shell, then $SHELL, then /bin/sh.
shell = ["nu", "-l"]
# Optional; defaults to ctrl+shift+r. Must not conflict with an app binding.
reload_bind = "ctrl+shift+r"
# Optional; defaults to a space. Must be one printable, one-cell character.
separator = "│"

[[app]]
name = "pi"
bind = "ctrl+shift+p"
cmd = ["pi"]
size = "half"

[[app]]
name = "lazygit"
bind = "ctrl+alt+g"
cmd = ["lazygit"]
size = "full"

[[app]]
name = "term"
bind = "ctrl+\\"
cmd = ["nu", "-l"]
```

Include every app you want to use in the config. `size` defaults to `half`; `full` is the other option. Commands are arrays of the executable and its arguments, not shell command strings. App names and bindings must be unique. Bindings support `ctrl`, `alt`, `shift`, and `super` modifiers, plus keys such as letters, `space`, `enter`, arrow keys and `f1`–`f24`.

## building

The Nix flake provides a package and development shell for `x86_64-linux`:

```sh
nix build
./result/bin/haunt
```

Alternatively, enter `nix develop` and run `cargo run` or `cargo build --release`. Outside the Nix shell, building needs Rust and either a discoverable libghostty-vt via pkg-config or Zig 0.15.x to build the vendored library.
