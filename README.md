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

## Building

Install Rust from <https://rustup.rs>, then:

```bash
./build.sh run
```

`./build.sh` alone makes a release build at `target/release/sample_manager`.
The script's header lists the other options (`dev`, `test`, `clean`).

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
(decoding), `hound` (WAV writing), `midir` (MIDI), `serde`, `trash` and
`walkdir`. Almost all of the dependency tree is MIT and/or Apache-2.0
licensed. Exceptions: `symphonia` is MPL-2.0 (file-level copyleft; we use
it unmodified), and egui's bundled default fonts are under the SIL Open Font
License and the Ubuntu Font Licence. Run `cargo metadata` or
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) for the full
list. No third-party file-browser library is used; the file browser is
written from scratch on top of `walkdir` and `trash`.

Sample Manager itself has not been given a licence yet.
