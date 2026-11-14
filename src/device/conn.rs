//! The MIDI link to the device: finding the port, the handshake, and one
//! request/answer at a time.

use super::*;

pub(super) struct Conn {
    pub(super) out: midir::MidiOutputConnection,
    pub(super) _input: midir::MidiInputConnection<()>,
    pub(super) rx: Receiver<Vec<u8>>,
    /// While a whole-project dump streams, API browser reads preserve the
    /// dump packets they encounter here for `collect_dump` to consume.
    pub(super) dump_pending: Option<VecDeque<Vec<u8>>>,
    pub(super) next: u16,
    pub(super) preset_writer: Option<u32>,
    pub(super) stereo: bool,
    /// Product id from the handshake (12 = Digitakt, 42 = Digitakt II).
    pub(super) product: u8,
    pub(super) name: String,
    /// The MIDI input port's own name, to notice when it disappears.
    pub(super) port: String,
    /// The port was found gone: every request fails from now on.
    pub(super) dead: bool,
    pub(super) probed: Instant,
}

/// Is a MIDI input called `name` still there? A fresh client each time:
/// a long-lived one may not see ports come and go without a run loop.
pub(super) fn port_present(name: &str) -> bool {
    let Ok(input) = midir::MidiInput::new("Sample Manager probe") else {
        return true;
    };
    input
        .ports()
        .iter()
        .any(|p| input.port_name(p).ok().as_deref() == Some(name))
}

/// Could this MIDI port be an Elektron machine?
pub(super) fn elektron_port(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [
        "elektron",
        "digitakt",
        "digitone",
        "syntakt",
        "rytm",
        "model:samples",
        "model:cycles",
        "analog four",
        "analog keys",
        "analog heat",
    ]
    .iter()
    .any(|k| l.contains(k))
}

/// Elektron product ids (the first byte of the device reply), as listed by
/// Elektroid: `(name, stereo samples)`. Whether a machine has a sample drive
/// is read from the commands it says it supports, not from here.
pub(super) fn product(id: u8) -> Option<(&'static str, bool)> {
    Some(match id {
        4 => ("Analog Four", false),
        6 => ("Analog Keys", false),
        8 => ("Analog Rytm", false),
        10 => ("Analog Heat", false),
        12 => ("Digitakt", false),
        14 => ("Analog Four MKII", false),
        16 => ("Analog Rytm MKII", false),
        20 => ("Digitone", false),
        22 => ("Analog Heat MKII", false),
        25 => ("Model:Samples", false),
        27 => ("Model:Cycles", false),
        28 => ("Digitone Keys", false),
        30 => ("Syntakt", false),
        32 => ("Analog Heat +FX", false),
        42 => ("Digitakt II", true),
        43 => ("Digitone II", false),
        _ => return None,
    })
}

/// USB MIDI ports can appear well before the device has finished booting.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
/// Repeat device identification in case a booting device ignored it.
const HANDSHAKE_RETRY_EVERY: Duration = Duration::from_secs(5);

impl Conn {
    /// Try each Elektron-looking port in turn; the first that answers and
    /// has a sample drive wins.
    pub(super) fn open() -> Result<Self, String> {
        let input = midir::MidiInput::new("Sample Manager").map_err(|e| e.to_string())?;
        let names: Vec<String> = input
            .ports()
            .iter()
            .filter_map(|p| input.port_name(p).ok())
            .filter(|n| elektron_port(n))
            .collect();
        drop(input);
        if names.is_empty() {
            return Err("No Elektron device found. Connect it by USB \
                        (USB config: Overbridge or USB MIDI) and press Refresh."
                .into());
        }
        let mut last = String::new();
        for name in names {
            match Self::open_port(&name) {
                Ok(c) => {
                    crate::log::line(format!("device: connected to {} on port {name}", c.name));
                    return Ok(c);
                }
                Err(e) => {
                    crate::log::line(format!("device: port {name}: {e}"));
                    last = e;
                }
            }
        }
        Err(last)
    }

    pub(super) fn open_port(in_name: &str) -> Result<Self, String> {
        let mut input = midir::MidiInput::new("Sample Manager").map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::None);
        let output = midir::MidiOutput::new("Sample Manager").map_err(|e| e.to_string())?;

        let in_port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).ok().as_deref() == Some(in_name))
            .ok_or("port went away")?;
        // The output of the same device carries the same name; failing that,
        // the only Elektron output there is.
        let outs: Vec<_> = output
            .ports()
            .into_iter()
            .filter_map(|p| Some((output.port_name(&p).ok()?, p)))
            .collect();
        let out_port = match outs.iter().find(|(n, _)| n == in_name) {
            Some((_, p)) => p.clone(),
            None => {
                let mut el = outs.iter().filter(|(n, _)| elektron_port(n));
                match (el.next(), el.next()) {
                    (Some((_, p)), None) => p.clone(),
                    _ => return Err("no matching MIDI output".into()),
                }
            }
        };

        let (tx, rx) = channel();
        let _input = input
            .connect(
                &in_port,
                "sample-manager-in",
                move |_, msg, _| {
                    if msg.first() == Some(&0xF0) {
                        let _ = tx.send(msg.to_vec());
                    }
                },
                (),
            )
            .map_err(|e| format!("Detected {in_name}, but could not open its MIDI input. A DAW or another MIDI app may be using it. Disable this device in that app or close the app, then click Retry connection. MIDI input: {e}"))?;
        let out = output
            .connect(&out_port, "sample-manager-out")
            .map_err(|e| format!("Detected {in_name}, but could not open its MIDI output. A DAW or another MIDI app may be using it. Disable this device in that app or close the app, then click Retry connection. MIDI output: {e}"))?;

        let mut c = Self {
            out,
            _input,
            rx,
            dump_pending: None,
            next: 0,
            preset_writer: None,
            stereo: false,
            product: 0,
            port: in_name.to_owned(),
            name: in_name.to_owned(),
            dead: false,
            probed: Instant::now(),
        };
        let r = c
            .request_within(proto::DEVICE, Args::default(), HANDSHAKE_TIMEOUT)
            .map_err(|e| format!(
                "Detected {in_name}, but it did not complete the MIDI handshake. Let the device finish booting, then click Retry connection. If it is ready, check USB config is Overbridge or USB MIDI and whether a DAW or another MIDI app is using it. Details: {e}"
            ))?;
        let mut rd = Reader::new(&r);
        let id = rd.u8().map_err(|e| format!("bad device reply: {e}"))?;
        // Message ids the device supports, then its name.
        let mut cmds = Vec::new();
        if let Ok(n) = rd.u8() {
            for _ in 0..n {
                cmds.extend(rd.u8());
            }
        }
        let known = product(id);
        c.name = match rd.str() {
            Ok(n) if !n.is_empty() => n,
            _ => known.map_or_else(
                || format!("Elektron device {id}"),
                |k| format!("Elektron {}", k.0),
            ),
        };
        crate::log::line(format!(
            "device: {} (product {id}) on {in_name}, commands {cmds:02x?}",
            c.name
        ));
        // An older firmware may not list its commands; then just try.
        let needed = [proto::DIR_LIST, proto::READ_OPEN, proto::WRITE_OPEN];
        if !cmds.is_empty() && !needed.iter().all(|n| cmds.contains(n)) {
            return Err(format!("{} has no sample drive to browse", c.name));
        }
        c.stereo = known.is_some_and(|k| k.1);
        c.product = id;
        Ok(c)
    }

    /// At most every [`PROBE_EVERY`], check the port is still there.
    /// Returns false once it is gone.
    pub(super) fn probe(&mut self) -> bool {
        if !self.dead && self.probed.elapsed() >= PROBE_EVERY {
            self.probed = Instant::now();
            if !port_present(&self.port) {
                crate::log::line(format!("device: port {} disappeared", self.port));
                self.dead = true;
            }
        }
        !self.dead
    }

    pub(super) fn request(&mut self, cmd: u8, args: Args) -> R<Vec<u8>> {
        self.request_within(cmd, args, TIMEOUT)
    }

    pub(super) fn request_within(&mut self, cmd: u8, args: Args, timeout: Duration) -> R<Vec<u8>> {
        if self.dead {
            return Err(Fail::Link("Device disconnected".into()));
        }
        if proto::FORBIDDEN.contains(&cmd)
            && !elek::presets::allowed_edit(cmd, &args.0)
            && !elek::presets::allowed_write(cmd, &args.0, self.preset_writer) {
            return refused(format!(
                "command 0x{cmd:02x} would change saved projects; blocked"
            ));
        }
        self.next = self.next.wrapping_add(1).max(1);
        let id = self.next;
        while let Ok(msg) = self.rx.try_recv() {
            if let Some(pending) = &mut self.dump_pending {
                pending.push_back(msg);
            }
        }
        let frame = proto::frame(id, cmd, &args.0);
        self.out
            .send(&frame)
            .map_err(|e| Fail::Link(format!("MIDI send failed: {e}")))?;
        let deadline = Instant::now() + timeout;
        // Identification is read-only and safe to repeat. Keep the same id
        // so a late answer to any attempt is accepted. Never replay writes.
        let handshake = cmd == proto::DEVICE;
        let mut retry_at = Instant::now() + HANDSHAKE_RETRY_EVERY;
        loop {
            if handshake {
                if super::switched_off() {
                    return Err(Fail::Link("Device switched off".into()));
                }
                if !self.probe() {
                    return Err(Fail::Link("Device disconnected".into()));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                crate::log::line(format!("device: cmd 0x{cmd:02x} id {id} timed out"));
                return Err(Fail::Link("The device stopped answering (timed out)".into()));
            }
            let wait = if handshake {
                left.min(PROBE_EVERY).min(retry_at.saturating_duration_since(Instant::now()))
            } else {
                left
            };
            let msg = match self.rx.recv_timeout(wait) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => {
                    if handshake && Instant::now() >= retry_at && Instant::now() < deadline {
                        if super::switched_off() {
                            return Err(Fail::Link("Device switched off".into()));
                        }
                        if !self.probe() {
                            return Err(Fail::Link("Device disconnected".into()));
                        }
                        self.out.send(&frame)
                            .map_err(|e| Fail::Link(format!("MIDI send failed: {e}")))?;
                        retry_at = Instant::now() + HANDSHAKE_RETRY_EVERY;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Fail::Link("Device MIDI input closed".into()));
                }
            };
            if let Some((c, a)) = proto::unframe(&msg, id) {
                if c == cmd | 0x80 {
                    return Ok(a);
                }
            }
            if let Some(pending) = &mut self.dump_pending {
                pending.push_back(msg);
            }
        }
    }

    /// For calls answering a single "ok" byte.
    pub(super) fn ok(&mut self, cmd: u8, args: Args, what: String) -> R<()> {
        let r = self.request(cmd, args)?;
        if Reader::new(&r).bool().unwrap_or(false) {
            Ok(())
        } else {
            crate::log::line(format!(
                "device: cmd 0x{cmd:02x} refused ({what}), reply {r:02x?}"
            ));
            refused(what)
        }
    }

    pub(super) fn list(&mut self, p: &Path) -> R<Vec<DirEntry>> {
        let r = self.request(proto::DIR_LIST, Args::default().str(&dev_str(p)))?;
        proto::parse_listing(&r).map_err(Fail::Link)
    }

    pub(super) fn mkdir(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(
            proto::DIR_CREATE,
            Args::default().str(&s),
            format!("could not create folder {s}"),
        )
    }

    /// Remove an empty folder.
    pub(super) fn rmdir(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(
            proto::DIR_DELETE,
            Args::default().str(&s),
            format!("could not remove folder {s}"),
        )
    }

    pub(super) fn delete_file(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(
            proto::FILE_DELETE,
            Args::default().str(&s),
            format!("could not delete {s}"),
        )
    }

    pub(super) fn rename(&mut self, from: &Path, to: &Path) -> R<()> {
        let (a, b) = (dev_str(from), dev_str(to));
        self.ok(
            proto::ITEM_RENAME,
            Args::default().str(&a).str(&b),
            format!("could not move {a} to {b}"),
        )
    }

    /// `on_bytes(conn, chunk, file_total)` after each chunk.
    pub(super) fn read(
        &mut self,
        p: &Path,
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(&mut Conn, u64, u64),
    ) -> R<Vec<u8>> {
        let s = dev_str(p);
        let r = self.request(proto::READ_OPEN, Args::default().str(&s))?;
        let mut rd = Reader::new(&r);
        let ok = rd.bool().map_err(Fail::Link)?;
        let fd = rd.u32().map_err(Fail::Link)?;
        let total = rd.u32().map_err(Fail::Link)? as usize;
        if !ok {
            return refused(format!("could not open {s}"));
        }
        let mut data = Vec::with_capacity(total);
        let res = (|| {
            while data.len() < total {
                if cancel.load(Ordering::Relaxed) {
                    return refused("cancelled");
                }
                let len = (total - data.len()).min(proto::CHUNK) as u32;
                let start = data.len() as u32;
                // The drive sometimes answers a chunk with "not ok" or no
                // data; asking again for the same chunk usually works.
                let mut tries = 0;
                let chunk = loop {
                    let r =
                        self.request(proto::READ, Args::default().u32(fd).u32(len).u32(start))?;
                    let mut rd = Reader::new(&r);
                    let ok = rd.bool().map_err(Fail::Link)?;
                    let _fd = rd.u32().map_err(Fail::Link)?;
                    let _len = rd.u32().map_err(Fail::Link)?;
                    let _start = rd.u32().map_err(Fail::Link)?;
                    let _end = rd.u32().map_err(Fail::Link)?;
                    let chunk = rd.rest();
                    if ok && !chunk.is_empty() {
                        break chunk.to_vec();
                    }
                    tries += 1;
                    crate::log::line(format!(
                        "device: read chunk at {start} of {s} failed (try {tries}, ok={ok}, {} bytes)",
                        chunk.len()
                    ));
                    if tries >= CHUNK_TRIES || cancel.load(Ordering::Relaxed) {
                        return refused(format!("read failed: {s}"));
                    }
                    std::thread::sleep(Duration::from_millis(100 * tries as u64));
                };
                data.extend_from_slice(&chunk);
                on_bytes(&mut *self, chunk.len() as u64, total as u64);
            }
            Ok(())
        })();
        let close = self.request(proto::READ_CLOSE, Args::default().u32(fd));
        res?;
        close?;
        data.truncate(total);
        Ok(data)
    }

    /// `on_bytes(conn, chunk, file_total)` after each chunk.
    pub(super) fn write(
        &mut self,
        p: &Path,
        data: &[u8],
        cancel: &AtomicBool,
        on_bytes: &mut dyn FnMut(&mut Conn, u64, u64),
    ) -> R<()> {
        let s = dev_str(p);
        let total = data.len() as u32;
        let r = self.request(proto::WRITE_OPEN, Args::default().u32(total).str(&s))?;
        let mut rd = Reader::new(&r);
        let ok = rd.bool().map_err(Fail::Link)?;
        let fd = rd.u32().map_err(Fail::Link)?;
        if !ok {
            return refused(format!("could not create {s} (drive full, or bad name?)"));
        }
        let res = (|| {
            let mut off = 0usize;
            for chunk in data.chunks(proto::CHUNK) {
                if cancel.load(Ordering::Relaxed) {
                    return refused("cancelled");
                }
                let args = Args::default()
                    .u32(fd)
                    .u32(chunk.len() as u32)
                    .u32(off as u32)
                    .bytes(chunk);
                let r = self.request(proto::WRITE, args)?;
                if !Reader::new(&r).bool().unwrap_or(false) {
                    return refused(format!("write failed: {s}"));
                }
                off += chunk.len();
                on_bytes(&mut *self, chunk.len() as u64, total as u64);
            }
            Ok(())
        })();
        match res {
            Ok(()) => {
                let r = self.request(proto::WRITE_CLOSE, Args::default().u32(fd).u32(total))?;
                if Reader::new(&r).bool().unwrap_or(false) {
                    Ok(())
                } else {
                    refused(format!("could not finish {s}"))
                }
            }
            Err(e) => {
                // Closing with length 0 abandons the file; then remove the stub.
                if self
                    .request(proto::WRITE_CLOSE, Args::default().u32(fd).u32(0))
                    .is_ok()
                {
                    let _ = self.delete_file(p);
                }
                Err(e)
            }
        }
    }
}

/// Rename `from` to `to`, refusing to replace anything already there.
pub(super) fn rename_checked(c: &mut Conn, from: &Path, to: &Path) -> R<()> {
    let parent = to.parent().unwrap_or(Path::new("/"));
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if c.list(parent)?.iter().any(|e| e.name == name) {
        return refused(format!("{} already exists", dev_str(to)));
    }
    c.rename(from, to)
}

pub(super) fn entries(path: &Path, v: Vec<DirEntry>) -> Vec<crate::fs_model::Entry> {
    v.into_iter()
        .map(|d| crate::fs_model::Entry {
            path: path.join(&d.name),
            name: d.name,
            is_dir: d.is_dir,
            len: if d.is_dir { 0 } else { d.size as u64 },
            modified: None,
            is_audio: false,
            empty: false,
            hash: d.hash,
        })
        .collect()
}
