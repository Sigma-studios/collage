# collage

A clipboard manager for KDE Plasma on Wayland, written in Rust. It replaces Klipper.

- **A toast every time you copy:** a small "Copied · preview…" pill at the bottom center of the screen. It's click-through and fades out.
- **Meta+S** saves the **highlighted text** as a note. It works in every app, terminals included, and never touches your clipboard.
- **Meta+V** opens a searchable popup with two tabs: **History** (what you copied) and **Notes** (what you saved). Each item shows its date. Press Enter to put an item back on the clipboard.
- **Tray icon:** click it to open the popup.
- **Keeps the clipboard alive** after the app you copied from closes.
- **Password managers:** copies marked as passwords (KeePassXC etc.) are never stored.

## Install (NixOS + Home Manager)

```nix
# flake.nix
inputs.collage.url = "path:/path/to/collage";   # or a git URL

# home.nix
imports = [ inputs.collage.homeManagerModules.default ];
services.collage.enable = true;
# services.collage.settings = { history_limit = 1000; };   # optional, see config.example.toml
```

This installs `collage` and runs `collage daemon` as a systemd user service tied to your graphical session.

Without Home Manager: `nix build` gives you `./result/bin/collage`. Run `collage daemon` at login.

## Plasma setup (one time)

1. **Disable Klipper:** right-click the system tray → *Configure System Tray* → *Entries* → **Clipboard: Disabled**.
2. **Free Meta+V:** *System Settings → Shortcuts → Plasma Workspace*: remove the Meta+V binding from the clipboard entry.
   Check that Meta+S is free too: `grep -i 'meta+s[,\t]' ~/.config/kglobalshortcutsrc` should print nothing.
3. **Add the two shortcuts:** *System Settings → Shortcuts → Add New → Command or Script*:
   - `collage popup` → **Meta+V**
   - `collage note` → **Meta+S**

With [plasma-manager](https://github.com/nix-community/plasma-manager), step 3 is:

```nix
programs.plasma.hotkeys.commands = {
  collage-popup = { name = "Collage: history & notes"; key = "Meta+V"; command = "collage popup"; };
  collage-note  = { name = "Collage: save selection as note"; key = "Meta+S"; command = "collage note"; };
};
```

## Using it

| Where | Key | Action |
|---|---|---|
| anywhere | Ctrl+C / Ctrl+Shift+C | normal copy, plus a toast |
| anywhere | **Meta+S** | save the highlighted text as a note |
| anywhere | **Meta+V** | open the popup (press again to close it) |
| popup | type | search (all words must match) |
| popup | ↑ ↓ / PgUp PgDn | move the selection |
| popup | Enter / click | copy to the clipboard and close |
| popup | Tab | switch between History and Notes |
| popup | Shift+Del / ✕ | delete the entry |
| popup | Esc / click outside | close |

CLI commands: `collage list` (notes), `collage list --history`, `collage copy <id>`, `collage clear-history`, `collage quit`.

Data is stored in `~/.local/share/collage/collage.db` (SQLite). Config is `~/.config/collage/config.toml`; see `config.example.toml`.

## How it works

- `collage daemon` watches the clipboard and the primary selection (highlighted text) through the **wlr-data-control** Wayland protocol, which KWin supports. It saves new text copies to history and draws the toast on a click-through **layer-shell** surface (tiny-skia + cosmic-text, CPU only).
- `collage note` asks the daemon to read the current primary selection. That's the text you highlighted; middle-click pastes the same thing. So no copy keystroke is sent and the clipboard isn't touched.
- `collage popup` is a separate short-lived process (iced + iced_layershell). It reads the database directly and asks the daemon to set the clipboard, so the copied text stays available after the popup closes.

## Troubleshooting

- Run `RUST_LOG=collage=debug collage daemon` in a terminal to see what's happening. Stop the service first with `systemctl --user stop collage`.
- `wayland-info | grep data_control` should list `zwlr_data_control_manager_v1`, version 2 or higher, for Meta+S to work.
- The popup uses the GPU (wgpu). If it misbehaves, try `ICED_BACKEND=tiny-skia collage popup`.
- A few apps don't set the primary selection (some Electron apps). Meta+S can't see their highlighted text; the toast says "Nothing selected".

## Developing

`nix develop`, then `cargo run -- daemon`. To type-check for Linux from macOS:
`rustup target add x86_64-unknown-linux-gnu && cargo check --target x86_64-unknown-linux-gnu`. The build scripts need fake `xkbcommon`, `sqlite3` and `wayland-client` pkg-config files plus `PKG_CONFIG_ALLOW_CROSS=1`.
