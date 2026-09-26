# clip-convert

A tray application that acts on whatever is in the clipboard.

**A global hotkey (`Ctrl+B` by default) opens a menu of actions** for the
current clipboard content — different actions for a URL, for text, for an
image, for a selection of files and for video.

Which actions exist, what they are called, in what order they appear and what
they do is entirely config-driven. Adding your own is a few lines of TOML and a
script; nothing needs recompiling.

It began as a fork of [`yourls-tray-app`](../yourls-tray-app), but shares no
code with it any more. Shortening went back the other way: that app is now a
CLI, and this one calls it like any other configured command.

---

## Screenshots

<details>
<summary>What it looks like</summary>

| | |
| :---: | :---: |
| ![The action menu for an image](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/actions_picture.png) | ![The action menu for a file selection](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/actions_files_text.png) |
| **An image on the clipboard.** The menu names what it found — kind, format, dimensions, size — and offers only what can act on it. | **Five files, which are also text.** A selection has a text form too, so the text actions are offered for the list of paths. |
| ![The resize form with a preset armed](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/presets_image_resize.png) | ![Converting a PNG](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/convert_picture.png) |
| **Size presets.** Clicking one fills its limits into the form below and tints the button; clicking it again applies it. Background removal and cropping are on by default for any format that can hold transparency. | **Conversions.** Offered only when something on the clipboard can actually be converted, and the list is whatever the config says. |
| ![Find and replace](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/find_and_replace.png) | ![Re-encoding a video](https://raw.githubusercontent.com/Bluscream/clip-convert/main/assets/screenshots/resize_video.png) |
| **Find and replace.** A regular expression with capture groups; both fields remember what you last used. | **Video.** A different question from a sticker preset — width, height, file size and length, any of which may be left blank. |

</details>

## Installing

**Linux — AppImage.** Download `clip-convert-<version>-x86_64.AppImage` from the
[releases](https://github.com/Bluscream/clip-convert/releases), make it
executable and run it. It is built against glibc 2.31, so it runs on anything
from about 2020 onwards, and it carries almost nothing with it: OpenSSL is
compiled in, and Wayland, X11, xkbcommon and OpenGL are all loaded from the
system at runtime.

```bash
chmod +x clip-convert-*.AppImage
./clip-convert-*.AppImage
```

With [Gear Lever](https://github.com/mijorus/gearlever) installed, opening the
file with it instead installs it into `~/.local/bin`, adds the desktop entry
and takes care of updates.

To start it with the session:

```bash
systemctl --user enable --now clip-convert.service
```

The unit is in [`packaging/clip-convert.service`](packaging/clip-convert.service).
Use *one* autostart mechanism — a systemd unit and a `~/.config/autostart`
entry will both start it, and the second copy exits immediately because of the
single-instance lock, which looks exactly like a failure to start.

**Windows and macOS.** Both compile, and the release workflow builds and
attaches them — but nothing more than that: see
[platform support](#platform-support). They are not in the current releases,
because GitHub Actions does not run on this repository while it is private.
Making it public is all that is needed.

---

## The action menu

Press the hotkey and the menu lists what applies to the current content:

| Action | Shown for | What it does |
| :--- | :--- | :--- |
| **Type** | anything | Types the text into the focused window one character at a time, so it lands in applications that ignore a paste. |
| **Split** | text, a URL | Asks for a character limit, then sends the text in pieces, pressing Return between them and pausing so the receiving app keeps up. |
| **Truncate** | text, a URL | Asks for a character limit and cuts the text to fit, marking the cut. |
| **Replace** | text, a URL, rich text | Asks for a regular expression and a replacement — `$1` and `${name}` work — and substitutes every match. The last ten of each are remembered and offered beside the field. |
| **Resize** | an image, video | For images, asks for a size target — a platform preset or your own. For video, asks for any of width, height, file size and length. |
| **Convert** | an image, files, rich text | Asks what to convert to, in a second dialog listing only what the current content can actually become. |

Labels name what they will act on, pluralised by what is there: the same entry
reads *Resize Image*, *Resize Images* or *Resize Videos*.

The menu heading says what is on the clipboard — `3 files · images · 4.1 MB ·
also as text`. A **Paste after action** checkbox is remembered between uses.

Everything is reachable from the keyboard, because the hotkey that opened the
menu was pressed by hands that are already on it:

| Key | |
| :--- | :--- |
| `1`–`9` | Run that entry |
| `↑` `↓` `Tab` | Move the selection |
| `Enter` `Space` | Run the selected entry, or accept a form |
| `Esc` | Dismiss |

Only actions that can actually do something appear. An image gets *Resize* and
*Convert*; it does not get *Type*, because there is no text to type, and an
entry whose only outcome is an error box wastes the click the menu exists to
save. The same rule hides *Convert* when nothing can convert the format in
hand. It is applied only
where it makes sense: an action that has already typed its result into the
window is not pasted again.

### One clipboard, several kinds at once

A clipboard does not hold one thing. Copying files in a file manager publishes
`text/uri-list` and nothing else; copying an image in a browser publishes the
image alongside rich text and the source URL. So content is modelled as a list
of *facets*, and an action is offered when **any** of them matches.

That is what lets one selection be several things at once: a folder of pictures
is *files* to one action, *images* to another and *text* to a third, and all
three appear in the same menu.

### Default actions

The tray has a **Default action** submenu, grouped by content kind. Setting one
makes the hotkey run it immediately, without showing the menu. It lasts for the
session only and is never written to the config — it is a shortcut for a run of
repetitive work, not a setting to forget you changed.

### Size presets

`Resize` offers presets transcribed from a sticker-limits table, each carrying
its platform's real rules — Discord, Telegram (and pack icons), Signal, WhatsApp
(and tray icons), VRChat, Slack, LINE and Matrix — plus a **Custom size** form.
They live in the config file and can be added to, edited or removed.

A preset need not constrain dimensions at all: **Discord file** caps the size at
10 MB and changes nothing else, keeping the source's own format and canvas.

A preset marked `fit = "exact"` pads the image onto exactly that canvas, because
the platform mandates it. One marked `fit = "inside"` only scales to fit.

Meeting a file-size cap is a search: quality is lowered first, then — for
`inside` presets only — the image is scaled down, with the next size estimated
from the last measurement rather than stepped down blindly. An `exact` preset
that cannot meet its cap **reports that** rather than quietly returning a
smaller image the platform would reject.

### Video

Video is the one thing not done in process. Every video crate on crates.io is a
binding to FFmpeg — there is no pure-Rust transcoder — and linking FFmpeg would
mean system headers on every platform for one action. So `ffmpeg` and `ffprobe`
are run as programs, named in `[video]`.

Every field of the form is optional and only what is filled in is constrained.
Sizes are read the way people write them (`10mb`, `512k`, `1.5gb`), lengths as
`90`, `90s`, `1:30` or `1m30s`.

A file size becomes a bitrate from the video's own length, probed only when a
size was actually asked for. The budget is taken from the length that will be
**kept**, so cutting a clip buys quality rather than throwing it away, and a
size that cannot even hold the audio is refused with a reason rather than
encoded and silently overrun.

### Conversions

What can become what is a table in the config file. Natively: any image format
to any other — PNG, JPEG, WebP, GIF, BMP, TIFF, ICO — and HTML to Markdown. An
image too large for an ICO is scaled to fit rather than refused.

An entry with a `command` shells out instead, with `{input}` and `{output}`
standing in for the paths; a command that prints its result rather than writing
a file has its output captured, which is how most text converters behave. The
factory table ships one such entry, PDF to text via poppler's `pdftotext`,
disabled so it is discoverable but never fails unasked.

```toml
[[conversions]]
id = "svg-to-png"
from = ["svg"]
to = "png"
command = ["rsvg-convert", "-o", "{output}", "{input}"]
```

Only conversions the current content can feed are offered, and never one to the
format it already is.

---

## Configuration

Written on first run to `~/.config/clip-convert/config.toml`
(`%APPDATA%` on Windows), or to a `config.toml` beside the executable if you
want a portable copy. Edit it from the tray, then **Reload configuration**.

A misspelled key is an error at load time rather than a setting that silently
does nothing, and a bad edit is reported without disturbing the running config.

### Top level

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `paste_after_action` | `true` | Remembered state of the dialog checkbox. |
| `hotkey` | `"ctrl+b"` | `ctrl`, `shift`, `alt`, `meta`/`super`/`win`/`cmd`, plus a letter, digit, `f1`–`f12`, or a name such as `space`. |
| `notifications` | `true` | Show a desktop notification after an action. |
| `ignore_ssl_errors` | `false` | Accept invalid certificates when fetching an icon. |
| `type_delay_ms` | `12` | Per-key delay when typing. Raise it for apps that drop keys. |
| `focus_restore_delay_ms` | `250` | Pause after the dialog closes, so the compositor can hand focus back before typing. |

`[split]` takes `default_limit`, `delay_ms` and `press_enter`.
`[truncate]` takes `default_limit` and `ellipsis`.
`[replace]` takes `history_limit` and the remembered `patterns` and
`replacements` — written by the app, but reasonable to seed by hand.
`[video]` takes `ffmpeg`, `ffprobe`, `codec`, `audio_bitrate_kbps`, `container`
and `extra_args`.

`window_sizes` records how big each dialog was last left. Every window is
resizable and reopens the size you left it.

### Shortening a URL

Not built in. It is an ordinary command action — which is the point of the
action model, and is how the companion `yourls` CLI is wired up:

```toml
[[actions]]
id = "shorten"
label = "Shorten"
when = ["url"]
command = ["/home/you/.local/bin/yourls"]
input = "stdin"           # the URL goes in
output = "clipboard"      # the short URL comes back
```

### Your own actions

An action is either a named built-in or an external command. Order in the file
is the order in the menu.

```toml
[[actions]]
id = "ocr"
label = "Copy text from image"
when = ["image"]          # url, text, image, or any
command = ["tesseract", "-", "-"]
input = "stdin"           # stdin | argument | file | none
output = "clipboard"      # clipboard | type | notify | discard
```

- `input` decides how the content reaches the command. `stdin` is the default
  and the only one that handles image data and long text safely.
- `output` decides what happens to what the command prints.
- Set `enabled = false` to keep an entry but hide it.
- `when` also accepts `files`, `video` and `html`.

### Icons and colours

Actions, size presets and conversions may each carry an `icon` and their own
`button_color` and `text_color`:

```toml
[[actions]]
id = "type"
label = "Type"
builtin = "type"
icon = "/usr/share/icons/hicolor/48x48/apps/keyboard.png"
button_color = "#3b5bdb"
text_color = "#ffffff"
```

An icon may be written as base64, a `data:` URI, an `http(s)` URL or a local
path. Anything that is not already base64 is fetched or read **once** and
written back to the config as base64, so the entry keeps working after the
source moves and opening a menu never touches the network or the disk.

Colours are CSS-style hex — `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, with or
without the `#`. A value that is not a colour is reported when the config
loads, naming the entry and the field.

The command is an **argument vector, never a shell string**. Clipboard content
is untrusted input, and passing it through a shell would make anything you copy
a command injection. Every command is run with a timeout and its streams pumped
separately, so a misbehaving one cannot wedge the app.

To change a built-in, edit its entry: relabel it, reorder it, restrict its
`when`, or disable it and add your own in its place.

### Replacing the input backend

Everything is native by default and `[commands]` is empty. It exists for Wayland,
where an ordinary application is not permitted to synthesise input:

```toml
[commands]
type_text = ["ydotool", "type", "--key-delay", "{delay}", "--file", "-"]
key_enter = ["ydotool", "key", "28:1", "28:0"]
key_paste = ["ydotool", "key", "29:1", "47:1", "47:0", "29:0"]
```

---

## How it is put together

Two binaries:

- **`clip-convert`** — the daemon. Tray, hotkey, clipboard, actions. Links no
  graphical toolkit at all.
- **`clip-convert-dialog`** — draws one dialog and exits, taking a JSON request
  on stdin and printing a JSON reply.

They are split because Wayland has no operation for hiding a window — winit's
`set_visible` is a documented no-op there — so any long-lived GUI process shows
something for the whole session. With no toolkit in the daemon there is nothing
to hide, and nothing is held on the graphics driver while it sits in the tray.

Everything is event-driven: the keyboard is read through blocking reads, and
the clipboard is touched only when the hotkey is pressed. Measured idle, with
nothing happening:

```
idle CPU:    0.00% of a core
RSS:         10 MB
descriptors: flat
windows:     0
```

Three crates:

| | |
| :--- | :--- |
| `crates/clip-convert-core` | All the logic: config, the facet content model, actions, images, conversions, video, replacements. No UI dependencies, so it builds and tests without a desktop. |
| `crates/clip-convert-dialog` | The dialog process. |
| `src/` | The daemon. |

---

## Platform support
<a id="platform-support"></a>

| | Linux | Windows | macOS |
| :--- | :--- | :--- | :--- |
| Clipboard, images, conversions, actions | ✅ | ✅ | ✅ |
| Dialogs | ✅ | ✅ | ✅ |
| Tray | ✅ ksni | ⚠️ `tray-icon` | ⚠️ see below |
| Hotkey | ✅ evdev | ⚠️ `global-hotkey` | ⚠️ `global-hotkey` |
| Typing | ⚠️ see below | ✅ | ✅ |

**Only Linux has actually been run.** Windows and macOS compile and their
backends are written, but are unverified — treat them as a starting point.

Known platform limits, stated rather than papered over:

- **Wayland forbids synthesising input.** `Type`, `Split` and *Paste after
  action* need `ydotool` (or another tool) configured under `[commands]`. The
  error says so when it happens.
- **Wayland forbids global shortcuts**, so the hotkey is read from evdev
  directly. That needs membership of the `input` group:
  `sudo usermod -aG input $USER`, then log back in. The app reports it at
  startup if not.
- **macOS requires the tray on the main thread**, which is an unresolved
  conflict with how the daemon starts it. The app runs without a tray icon
  rather than pretending to work.

---

## Building

GTK is gone, but the dialog still needs the usual desktop development headers.
On an immutable host they live in a container:

```bash
./scripts/build.sh            # format, lint, test, docs, build
./scripts/build.sh --release  # optimised
./scripts/build.sh --probe    # also measure idle CPU, memory and descriptors
./scripts/appimage.sh         # the portable AppImage, built in Ubuntu 20.04
```

Check the exit status directly rather than piping the output somewhere: a
pipeline reports its *last* command's status, so `./scripts/build.sh | grep …`
succeeds even when the build fails.

Every step runs at `nice 15` on half the cores, so a build never makes the
machine unusable. Override with `LCC_JOBS` and `LCC_NICE`.

The core crate has no UI dependencies and is checked on the host first, which
takes seconds:

```bash
cargo test -p clip-convert-core --no-default-features
```

`--probe` is the only check that catches a runtime regression such as a
busy-wait loop or a descriptor leak. It is worth running before a release.

The clipboard end-to-end tests are the ones that catch what unit tests cannot —
they write to the real clipboard and read it back, which is how the image
re-encoding, the lost writes and the watcher that caused them were all found:

```bash
cargo test -p clip-convert-core --no-default-features \
    --test clipboard_end_to_end -- --ignored --test-threads=1
```

`tests/source_size.rs` fails the build for any file over 1000 lines or any
function over 100, and names what went over. Clippy has no lint for either.

One test is ignored by default because it needs `ffmpeg` installed: it encodes a
real video and checks the result lands inside its size cap. Run it after
touching anything that builds the encoder's arguments:

```bash
cargo test -p clip-convert-core --no-default-features -- --ignored
```
