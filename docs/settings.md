# Settings

md-tex reads its settings from a TOML file:

- macOS: `~/Library/Application Support/md-tex/settings.toml`
- Linux: `$XDG_CONFIG_HOME/md-tex/settings.toml`, or `~/.config/md-tex/settings.toml` when `XDG_CONFIG_HOME` is unset

The file does not exist until you change a setting, from the command palette or by creating it yourself. The app writes only the key you change and keeps your other lines and comments. Edits you make in another editor apply to every open window within half a second.

A key with an unknown name, a wrong type or an out-of-range value is ignored, and a notice names it; the other keys still apply. If the file is not valid TOML, the previous settings stay in effect and a notice gives the line of the error.

## theme

`"light"`, `"dark"` or `"system"`. `system` follows the operating system's appearance as it changes. Default: `"system"`.

## zoom

Whole number from 50 to 300, in percent. Cmd/Ctrl+Plus, Cmd/Ctrl+Minus and Cmd/Ctrl+0 change it and save it here. Default: `100`.

## remote_images

`"allow"` or `"block"`. When blocked, images from `http`/`https` URLs are not fetched and show a placeholder with a load action. Default: `"block"`.

## soft_wrap

`true` or `false`. Wraps long lines in the source editor at the window width. Default: `true`.

## cursor_blink

`true` or `false`. Default: `true`.

## initial_view_mode

`"reader"`, `"source"` or `"split"`: the view a newly opened document starts in. The palette's Settings: Start in Reader, Start in Source and Start in Split commands set it. Default: `"reader"`.

## wide_reader

`true` or `false`. Widens the reader's text column from 72 to 108 characters, so lines run half as long again; a window narrower than that still wraps at its own width. Cmd+Shift+L (Ctrl+Shift+L on Linux), the Wide button in the view switcher at the top of the window, and View > Wide Reader on macOS toggle it and save it here. Default: `false`.

## Example

```toml
theme = "dark"
zoom = 125
remote_images = "block"
soft_wrap = true
cursor_blink = false
initial_view_mode = "split"
wide_reader = true
```
