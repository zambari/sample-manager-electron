//! Cut, copy, paste, insert silence, slice markers, and the splice they all go through.

use super::*;

impl Editor {
    /// Whether the selection is "everything shown", i.e. nothing picked out.
    pub(super) fn whole_selected(&self) -> bool {
        self.sel == (0, self.frames())
    }

    /// Copy the selection, with gain, to the editor clipboard.
    pub fn copy(&mut self) -> bool {
        let Some(s) = self.sample() else { return false };
        let (a, b) = self.sel;
        let g = self.gain();
        let channels = s
            .channels
            .iter()
            .map(|c| c[a..b].iter().map(|x| x * g).collect())
            .collect();
        let name = self.source_name(a);
        self.clip = Some(AudioClip {
            rate: s.rate,
            channels,
            name,
        });
        true
    }

    /// Whether removing the selection would leave some audio behind.
    pub fn can_remove(&self) -> bool {
        let Some(full) = &self.full else { return false };
        self.current.is_some() && full.sample.frames() > self.sel.1 - self.sel.0
    }

    pub fn cut(&mut self) -> bool {
        if !self.can_remove() || !self.copy() {
            return false;
        }
        self.delete()
    }

    /// Remove the selection without touching the clipboard.
    pub fn delete(&mut self) -> bool {
        if !self.can_remove() {
            return false;
        }
        let (a, b) = self.sel;
        self.splice(a, b, &[]);
        self.settle_grid();
        true
    }

    /// The selection as you hear it (gain, and mono when it's on), for a
    /// slice kit. Leaves the clipboard alone.
    pub fn slice(&self) -> Option<AudioClip> {
        let s = self.sample()?;
        let (a, b) = self.sel;
        if b <= a {
            return None;
        }
        let g = self.gain();
        let channels = match self.fold() {
            Some(f) => vec![
                stats::fold_region(&s.channels, a, b, f)
                    .into_iter()
                    .map(|x| x * g)
                    .collect(),
            ],
            None => s
                .channels
                .iter()
                .map(|c| c[a..b].iter().map(|x| x * g).collect())
                .collect(),
        };
        Some(AudioClip {
            rate: s.rate,
            channels,
            name: self.source_name(a),
        })
    }

    /// Add `clip` after the end of the whole file (uncropping first), then
    /// show all of it. A grid kit puts it in its next free slot instead.
    pub fn append(&mut self, clip: &AudioClip) -> Result<(), String> {
        if let Some(n) = self.grid {
            return self.append_on_grid(clip, n);
        }
        let Some(full) = &self.full else {
            return Err("No kit loaded".to_owned());
        };
        let (rate, nch) = (full.sample.rate, full.sample.channels.len());
        self.uncrop();
        let ins = clip.adapt(rate, nch);
        let end = self.frames();
        self.splice(end, end, &ins);
        // The new slice starts where the file used to end.
        if self.is_kit && end > 0 && end < self.frames() {
            self.add_marker(end);
        }
        self.name_slice_at(end, clip.name.as_deref());
        self.view.fit_pending = true;
        Ok(())
    }

    /// Start a file that isn't on disk yet: `clip` at `path`, unsaved,
    /// laid out on `grid` slots if given.
    pub fn start_new(
        &mut self,
        path: PathBuf,
        clip: &AudioClip,
        bits: Option<u32>,
        grid: Option<usize>,
    ) {
        self.generation += 1; // drop any load still on its way
        self.loading = None;
        self.play_when_loaded = false;
        self.memos.remove(&path);
        let sample = Sample {
            path,
            rate: clip.rate,
            channels: clip.channels.clone(),
            bits,
            format: "WAV".into(),
            file_len: 0,
        };
        self.install(Loaded::new(sample), Vec::new(), Vec::new());
        self.name_slice_at(0, clip.name.as_deref());
        if grid.is_some() && self.set_grid(grid).is_ok() {
            self.sel = (0, self.markers.first().copied().unwrap_or(self.frames()));
            self.playhead = 0;
            // The layout is where the kit starts, not an undo step.
            self.history = History {
                committed: Some(self.state()),
                ..History::default()
            };
        }
        self.clean_rev = 0; // nothing written yet
    }

    /// Hand over this slot's file and edits, leaving it empty. The slot
    /// keeps its prefs, clipboard and kit flag.
    pub fn take(&mut self) -> Editor {
        let mut fresh = Editor::new(self.ctx.clone(), self.prefs.clone());
        fresh.clip = self.clip.take();
        fresh.is_kit = self.is_kit;
        std::mem::replace(self, fresh)
    }

    /// Empty the editor, keeping prefs and the clipboard.
    pub fn close(&mut self) {
        let clip = self.clip.take();
        let is_kit = self.is_kit;
        *self = Editor::new(self.ctx.clone(), self.prefs.clone());
        self.clip = clip;
        self.is_kit = is_kit;
    }

    pub fn can_paste(&self) -> bool {
        self.current.is_some() && self.clip.as_ref().is_some_and(|c| c.frames() > 0)
    }

    /// Insert the clipboard at the playhead, or over the selection if part
    /// of the file is selected. The playhead ends up after the pasted audio,
    /// so pasting again continues from there.
    pub fn paste(&mut self) -> bool {
        if !self.can_paste() {
            return false;
        }
        let (Some(s), Some(clip)) = (self.sample(), &self.clip) else {
            return false;
        };
        let ins = clip.adapt(s.rate, s.channels.len());
        let (a, b) = if self.whole_selected() {
            let p = self.playhead.min(self.frames());
            (p, p)
        } else {
            self.sel
        };
        let (fa, n) = self.splice(a, b, &ins);
        // In a kit, what was pasted is a slice of its own.
        if self.is_kit {
            self.add_marker(fa);
            self.add_marker(fa + n);
            let name = self.clip.as_ref().and_then(|c| c.name.clone());
            self.name_slice_at(fa, name.as_deref());
        }
        self.settle_grid();
        true
    }

    /// Insert `secs` of silence just before the playhead. Everything after
    /// it, slice markers included, moves later; the playhead stays on the
    /// audio it was on.
    pub fn insert_silence(&mut self, secs: f64) -> bool {
        let Some(s) = self.sample() else { return false };
        let n = (secs * s.rate as f64).round() as usize;
        if n == 0 {
            return false;
        }
        let zeros = vec![vec![0.0f32; n]; s.channels.len()];
        let p = self.playhead.min(self.frames());
        self.splice(p, p, &zeros);
        self.settle_grid();
        true
    }

    // ---- kit markers -------------------------------------------------------

    pub(super) fn add_marker(&mut self, f: usize) {
        let total = self.full.as_ref().map_or(0, |l| l.sample.frames());
        if f == 0 || f >= total {
            return;
        }
        if let Err(i) = self.markers.binary_search(&f) {
            self.names = self.slice_names();
            self.markers.insert(i, f);
            // The slice split here keeps its name on the left part.
            self.names.insert(i + 1, String::new());
        }
    }

    /// Every slice's name, one per slice (empty where unknown).
    pub(super) fn slice_names(&self) -> Vec<String> {
        let mut v = self.names.clone();
        v.resize(self.markers.len() + 1, String::new());
        v
    }

    /// Name the kit slice holding whole-file frame `f`.
    pub(super) fn name_slice_at(&mut self, f: usize, name: Option<&str>) {
        let Some(name) = name.filter(|n| !n.is_empty()) else { return };
        if !self.is_kit {
            return;
        }
        let i = self.markers.partition_point(|&m| m <= f);
        self.names = self.slice_names();
        self.names[i] = name.to_owned();
    }

    /// The kit slice under shown-sample frame `frame`: its 1-based number
    /// and the file it came from, when known.
    pub fn slice_name_at(&self, frame: usize) -> Option<(usize, &str)> {
        let f = frame + self.crop_offset();
        let i = self.markers.partition_point(|&m| m <= f);
        self.names
            .get(i)
            .filter(|n| !n.is_empty())
            .map(|n| (i + 1, n.as_str()))
    }

    /// What a clip taken from shown-sample frame `a` should be called: in a
    /// kit, the slice it starts in; otherwise this file's name.
    fn source_name(&self, a: usize) -> Option<String> {
        if self.is_kit {
            return self.slice_name_at(a).map(|(_, n)| n.to_owned());
        }
        self.path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    }

    /// Slice markers in shown-sample frames (inside the crop, if any).
    pub fn marks(&self) -> Vec<usize> {
        let off = self.crop_offset();
        let n = self.frames();
        self.markers
            .iter()
            .filter(|&&m| m >= off && m - off <= n)
            .map(|&m| m - off)
            .collect()
    }

    /// The slice around `frame`: from the marker at or before it to the next
    /// one (or the ends of what's shown).
    pub fn slice_at(&self, frame: usize) -> (usize, usize) {
        let n = self.frames();
        let marks = self.marks();
        let a = marks
            .iter()
            .copied()
            .filter(|&m| m <= frame)
            .max()
            .unwrap_or(0);
        let b = marks
            .iter()
            .copied()
            .filter(|&m| m > frame)
            .min()
            .unwrap_or(n);
        (a, b.max(a + 1).min(n.max(1)))
    }

    /// Replace `[a, b)` of the shown sample with `ins` (one Vec per channel).
    /// The edit goes into the whole file; a crop around it grows or shrinks
    /// with it. Afterwards nothing is selected and the playhead sits at the
    /// end of what went in.
    /// Returns where the new audio went in, in whole-file frames, and its length.
    pub(super) fn splice(&mut self, a: usize, b: usize, ins: &[Vec<f32>]) -> (usize, usize) {
        let Some(full) = &self.full else {
            return (0, 0);
        };
        let off = self.crop_offset();
        let (fa, fb) = (a + off, b + off);
        let n = ins.first().map_or(0, Vec::len);
        let fs = &full.sample;
        let channels: Vec<Vec<f32>> = fs
            .channels
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut v = Vec::with_capacity(c.len() - (fb - fa) + n);
                v.extend_from_slice(&c[..fa]);
                if let Some(x) = ins.get(i) {
                    v.extend_from_slice(x);
                }
                v.extend_from_slice(&c[fb..]);
                v
            })
            .collect();
        let sample = Sample {
            path: fs.path.clone(),
            rate: fs.rate,
            channels,
            bits: fs.bits,
            format: fs.format.clone(),
            file_len: fs.file_len,
        };
        let total = sample.frames();
        // Markers before the edit stay, ones inside what was replaced go,
        // the rest move with the audio after it.
        // Each marker carries the name of the slice it starts.
        let names = self.slice_names();
        let mut marks: Vec<(usize, String)> = self
            .markers
            .iter()
            .zip(names.iter().skip(1))
            .filter_map(|(&m, name)| {
                if m < fb && m <= fa {
                    Some((m, name.clone()))
                } else if m >= fb {
                    Some((m - (fb - fa) + n, name.clone()))
                } else {
                    None
                }
            })
            .filter(|&(m, _)| m > 0 && m < total)
            .collect();
        marks.dedup_by_key(|p| p.0);
        self.markers = marks.iter().map(|p| p.0).collect();
        self.names = std::iter::once(names[0].clone())
            .chain(marks.into_iter().map(|p| p.1))
            .collect();
        let new_full = Loaded::new(sample);
        self.full = Some(new_full.clone());
        self.next_rev += 1;
        self.rev = self.next_rev;
        self.refresh_file_stats();
        self.drag = None;
        self.stats_key = None;
        let caret = fa + n;
        match self.crop {
            Some((ca, cb)) if cb + n > ca + (fb - fa) => {
                let cb = cb + n - (fb - fa);
                self.sel = (ca, cb);
                self.playhead = caret;
                self.apply_crop(ca, cb);
                // apply_crop asks for a refit; keep the view where it is.
                self.view.fit_pending = false;
            }
            _ => {
                // No crop, or the whole crop was cut away.
                if self.crop.take().is_some() {
                    self.view.fit_pending = true;
                }
                self.current = Some(new_full);
                self.sel = (0, total);
                self.playhead = caret.min(total);
            }
        }
        (fa, n)
    }
}
