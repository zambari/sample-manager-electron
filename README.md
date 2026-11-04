# Sample Manager

A desktop app for organising audio samples and moving them onto an
Elektron Digitakt.

- **File browser** with list and tree views, an audio-only filter,
  favourites, cut/copy/paste, duplicate removal and reveal in Finder.
- **Sample editor** with playback, in/out points, looping, trim, gain,
  normalise and export.
- **Digitakt +Drive access over USB MIDI**: browse, create folders, rename,
  move, delete (to `/trash`, so mistakes can be undone), and transfer
  samples both ways.

Built in Rust with [egui](https://github.com/emilk/egui).

![Sample Manager: source browser, sample editor and destination browser](docs/screenshot1.png)

## How it works

The window is a sandwich: a **source** browser on the left, a
**destination** browser on the right, and a **sample editor** in between.
The idea is to dig through a large sample library quickly, cut out just the
bits you want, make them mono if needed, and drop them into a target
folder. That target can be another local folder or an Elektron machine's
+Drive.

**Source (left).** Where you browse your library. Tabs at the top hold
bookmarked folders (★ adds favourites, `+Drive` opens a connected Elektron
device). Sort by name, size, date or kind, type to filter, switch to a tree
view, or tick *All files* to list every file below the current folder.
Selecting a file loads it into the editor (and plays it, with *Auto-play*
on), so you can step through hundreds of samples with the arrow keys.

**Sample editor (centre).** Trim the loaded sample down to the part you
want. The *Show* row toggles panels on and off:

- **Transport**: play/stop, loop, follow playhead, snap to zero crossings,
  auto-load and auto-play.
- **Tools**: undo/redo, cut/copy/paste, save, crop/uncrop to the selection,
  halve or double the loop, and the channel mode: keep *Stereo*, *Mix* to
  mono, or keep only *L* or *R*.
- **Wave**: the main waveform. The green and red markers are the in and
  out points; the shaded part between them is the selection.
- **Scalpel**: a 40 ms close-up around the playhead for precise cuts.
- **Map**: the whole file at a glance; click to jump.
- **Gain**: gain and normalise.
- **Stats**: file statistics and a phase meter.

*Export → Destination* writes the selection, with the chosen channel mode,
into the folder open on the right.

**Destination (right).** The target folder, with the same tabs and
browsing tools as the source. It can be a local folder or the +Drive of a
Digitakt (or another Elektron sampler) over USB.

**Commit to destination** (top right) copies the source selection into the
active destination tab. If a single file is open in the editor, it is
written as you hear it: trimmed, with gain and mono settings applied, and
the original stays untouched. *Move* does the same but moves the files.

**Top bar.** Audio output device and its sample rate, and a toggle for
showing hidden files.

### Slice kits

A slice kit is a second file kept next to the sample in the editor. It
collects the bits you cut out of many samples, one after another, into a
single file (handy for chains of drum hits).

- Select part of a sample and press **K** (or click the arrow button) to
  add it to the end of the kit, as you hear it (trimmed, gain and mono
  applied).
- The first time, you're asked to name the new kit. It's created in the
  active destination tab when that's a local folder. To keep adding to a
  file you already have, open it and choose *Use as kit*.
- Press **B** to switch the editor between the sample and the kit. Moving
  to another file in the browser always brings you back to the sample.
- Press **⌘S** while showing the kit to save it. The kit stays open between
  launches, and quitting asks about unsaved kit changes.

### Drag and drop

- Drag files and folders between panes, onto folder rows, or onto a tab.
  Within the same drive a drop moves; hold **⌥** or **Ctrl** to copy
  instead (the cursor shows a copy badge). Drops between the computer and
  the Digitakt always copy.
- Drag files out to **Finder / Explorer**, or drag them in from there
  (dragging in always copies).
- Drop folders onto a tab bar to open each one as a new tab.

### Tabs and file operations

- Right-click a tab to **move or copy it to the other pane**. A long tab
  row scrolls sideways.
- Click an already-selected item again to rename it, Finder style.
- *New folder with selection* puts the selected items into a new folder
  and opens rename straight away.
- When a transfer hits an existing folder, *Merge* adds new files and skips
  identical ones.
- Transfers to and from the Digitakt run in a queue. Creating and renaming
  folders jumps ahead of queued file transfers, so the folder is ready
  before the files arrive.

## Building

Install Rust from <https://rustup.rs>, then:

```bash
./build.sh run
```

`./build.sh` alone makes a release build at `target/release/sample_manager`.
The script's header lists the other options (`bundle`, `windows`, `dev`,
`test`, `clean`).

**Windows from a Mac.** `./build.sh windows` cross-compiles a 64-bit `.exe`
into `target/bundle/Sample Manager Windows.zip`. It needs
[cargo-xwin](https://github.com/rust-cross/cargo-xwin)
(`cargo install cargo-xwin --locked`). The first build downloads Microsoft's
C runtime and Windows SDK into cargo-xwin's cache, which means accepting
Microsoft's license for them.

## Acknowledgements

The Digitakt support would not exist without **Mark Lentczner's
[elk-herd](https://github.com/mzero/elk-herd)**. Mark worked out Elektron's
undocumented SysEx file-transfer protocol and published his work as open
source. Our command codes, 7-bit encoding, transfer chunk size and the
`/trash` convention all come from studying his project. Thank you, Mark.¹

---

¹ **Licensing note.** Sample Manager has its own Rust implementation of the
protocol. It contains no elk-herd source code (elk-herd is written in Elm).
What we took from elk-herd is knowledge of how the protocol works, not its
code.

Sample Manager uses these third-party Rust crates, each under its own
licence: `eframe`/`egui` (UI), `cpal` (audio output), `symphonia`
(decoding), `hound` (WAV writing), `midir` (MIDI), `drag` and `mouse_position` (drag and drop with
Finder / Explorer), `serde`, `trash` and
`walkdir`. Almost all of the dependency tree is MIT and/or Apache-2.0
licensed. Exceptions: `symphonia` is MPL-2.0 (file-level copyleft; we use
it unmodified), and egui's bundled default fonts are under the SIL Open Font
License and the Ubuntu Font Licence. Run `cargo metadata` or
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) for the full
list. No third-party file-browser library is used; the file browser is
written from scratch on top of `walkdir` and `trash`.

Sample Manager itself is released under the [MIT License](LICENSE).
