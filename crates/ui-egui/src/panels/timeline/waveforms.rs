use super::{Align2, Arc, Color32, FilmcraftApp, ItemId, Rect, Tick, Tokens, TrackItem, pos2, vec2};

/// The height of a clip's waveform for a clip body `body_height` tall: the lower part of a tall clip,
/// under the name strip; the whole clip on a compact track (under 50 points), where a share of it
/// would be too small to read and the name is drawn over the waveform.
pub(super) fn waveform_zone_height(body_height: f32) -> f32 {
    if body_height < 50.0 { (body_height - 2.0).max(8.0) } else { (body_height * 0.42).max(8.0) }
}

pub(super) fn draw_waveform(app: &mut FilmcraftApp, p: &egui::Painter, body: Rect, it: &TrackItem, col: Color32) {
    let Some(peaks) = request_peaks(app, it.item, it.audio_stream) else { return };
    let pkey = (it.item, it.audio_stream);
    let spp = 256.0;
    let sr = 48_000.0;
    let zone_h = waveform_zone_height(body.height());
    let area = Rect::from_min_max(pos2(body.min.x + 1.0, body.max.y - zone_h - 1.0), pos2(body.max.x - 1.0, body.max.y - 1.0));
    let clip = p.clip_rect().intersect(area);
    if clip.width() <= 0.0 {
        return;
    }
    let key = Arc::as_ptr(&peaks) as usize;
    let peak = match app.tl.peak_max.get(&pkey) {
        Some(&(k, v)) if k == key => v,
        _ => {
            let v = peaks.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()));
            app.tl.peak_max.insert(pkey, (key, v));
            v
        }
    };
    let gain = waveform_display_gain(peak, it.gain_db);
    let dynamic = app.ui.extras.dynamic_waveforms;
    let mut mesh = egui::Mesh::default();
    let dur_px = body.width().max(1.0);
    for x in (clip.min.x.floor() as i32)..(clip.max.x.ceil() as i32) {
        let f0 = (x as f32 - body.min.x) / dur_px;
        let f1 = (x as f32 + 1.0 - body.min.x) / dur_px;
        let t0 = it.start + Tick((it.duration.0 as f64 * f0 as f64) as i64);
        let t1 = it.start + Tick((it.duration.0 as f64 * f1 as f64) as i64);
        let s0 = (it.source_time_at(t0).seconds() * sr / spp) as usize;
        let s1 = ((it.source_time_at(t1).seconds() * sr / spp) as usize).max(s0 + 1);
        let mut m = 0f32;
        for (a, b) in peaks.iter().skip(s0).take(s1 - s0) {
            m = m.max(a.abs()).max(b.abs());
        }
        // as in Premiere, the waveform shows the clip's Volume
        m *= crate::panels::timeline_volume::gain_at(it, t0);
        // View ▸ Dynamic Audio Waveforms (default): logarithmic scale, −48 dB → 0, 0 dB → full;
        // off: linear amplitude
        let h = if dynamic {
            let db = 20.0 * (m * gain).max(1e-5).log10();
            ((db + 48.0) / 48.0).clamp(0.0, 1.0) * area.height()
        } else {
            (m * gain).clamp(0.0, 1.0) * area.height()
        };
        if h > 0.3 {
            mesh.add_colored_rect(Rect::from_min_max(pos2(x as f32, area.max.y - h), pos2(x as f32 + 1.0, area.max.y)), col);
        }
    }
    p.add(mesh);
    // channel label box
    let cb = Rect::from_min_size(pos2(body.min.x + 3.0, body.max.y - 11.0), vec2(8.0, 9.0));
    if body.width() > 30.0 {
        p.rect_filled(cb, 1.0, Color32::from_black_alpha(160));
        p.text(cb.center(), Align2::CENTER_CENTER, "1", Tokens::ui(7.5), Color32::from_rgb(0xd9, 0xd9, 0xd9));
    }
}

/// Display gain for a clip's waveform: clip gain, plus normalisation of the source to full scale
/// so quiet recordings stay readable, but by at most +12 dB, so a near-silent track (room tone at
/// −50 dBFS, a silent film's empty audio stream) does not look like a loud one.
pub(super) fn waveform_display_gain(source_peak: f32, clip_gain_db: f64) -> f32 {
    const MAX_BOOST: f32 = 3.981_072; // +12 dB
    let boost = (1.0 / source_peak.max(1e-9)).min(MAX_BOOST);
    filmcraft_render::audio::db_to_gain(clip_gain_db) * boost
}

/// A key that changes when the sound of sequence `item` does: its audio segments (clips,
/// transitions, mixer state; see `filmcraft_render::preview::audio_segments`) and its length.
fn sequence_audio_key(project: &filmcraft_project::Project, item: ItemId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for seg in filmcraft_render::preview::audio_segments(project, item) {
        (seg.first_sample, seg.samples, seg.hash).hash(&mut h);
    }
    project.sequence(item).map(|q| (q.duration().0, q.settings.sample_rate)).hash(&mut h);
    h.finish()
}

/// A waveform pass that a read which had to wait (web: bytes still being fetched) interrupted:
/// the peaks so far and where to go on, so the next pass resumes there instead of starting over.
/// On the web the browser keeps at most `CACHE_BUDGET` (384 MiB, `apps/filmcraft-web/src/fs.rs`)
/// of a file's bytes, so a larger file could never be read whole in one pass.
#[derive(Default)]
pub struct PeakPass {
    /// What the peaks are of: the media's reopen count, or a sequence's sound key.
    generation: u64,
    out: Vec<(f32, f32)>,
    /// Next sample to read.
    next: usize,
    /// Passes in a row that waited before reading anything new.
    stalls: u32,
    /// Not before this (passes never run on consecutive frames once one had to wait).
    retry_at: Option<web_time::Instant>,
}

/// After this many passes in a row that read nothing new, the peaks so far are kept as they are.
const MAX_STALLS: u32 = 4;
/// Wait after a pass that had to wait: 25 ms, doubled for each pass in a row that read nothing new.
const RETRY_BASE_MS: u64 = 25;

impl PeakPass {
    fn new(generation: u64) -> Self {
        Self { generation, ..Default::default() }
    }

    fn waiting(&self) -> bool {
        self.retry_at.is_some_and(|t| web_time::Instant::now() < t)
    }

    /// Read on from `next` in `chunk`s of `total` samples; `step(start, n)` gives the peaks of a
    /// chunk (None: an error, the peaks end there). True when finished; false when a read had to
    /// wait (resume later from the same chunk).
    fn run(&mut self, total: usize, chunk: usize, mut step: impl FnMut(usize, usize) -> Option<Vec<(f32, f32)>>) -> bool {
        let earlier = filmcraft_media::pending::take();
        let mut progressed = false;
        let done = loop {
            if self.next >= total {
                break true;
            }
            let n = chunk.max(1).min(total - self.next);
            let r = step(self.next, n);
            if filmcraft_media::pending::take() {
                self.stalls = if progressed { 0 } else { self.stalls.saturating_add(1) };
                if self.stalls >= MAX_STALLS {
                    // bytes that never come: keep what was read
                    break true;
                }
                let wait = RETRY_BASE_MS.saturating_mul(1u64 << self.stalls.min(8));
                self.retry_at = Some(web_time::Instant::now() + std::time::Duration::from_millis(wait));
                filmcraft_media::pending::mark();
                return false;
            }
            match r {
                Some(p) => {
                    self.out.extend(p);
                    self.next += n;
                    progressed = true;
                }
                None => break true,
            }
        };
        if earlier {
            filmcraft_media::pending::mark();
        }
        done
    }
}

fn min_max(c: &[f32]) -> (f32, f32) {
    c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)))
}

/// Waveform peaks of a nested sequence's mix, in the same form as a media item's (min, max per
/// 256 samples at 48 kHz). They are mixed in the background, and again whenever the sequence's
/// sound changes; until the new ones are ready the old ones stay on show.
fn request_sequence_peaks(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
    let revision = app.session.revision;
    let key = match app.tl.seq_keys_seen.get(&item) {
        Some(&(rev, key)) if rev == revision => key,
        _ => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            // and again when media came back (`relink::attach`): it was mixed in as silence
            (sequence_audio_key(&app.session.project, item), app.session.media.reopened_total()).hash(&mut h);
            let key = h.finish();
            app.tl.seq_keys_seen.insert(item, (revision, key));
            key
        }
    };
    let pkey = (item, 0);
    let have = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner()).get(&pkey).cloned();
    let current = app.tl.seq_peak_keys.lock().unwrap_or_else(|e| e.into_inner()).get(&item) == Some(&key);
    if current && have.is_some() {
        return have;
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return have;
        }
        let parts = app.tl.peak_passes.lock().unwrap_or_else(|e| e.into_inner());
        if parts.get(&pkey).is_some_and(|p| p.generation == key && p.waiting()) {
            return have;
        }
        pend.push(pkey);
    }
    let (peaks, keys, pending, passes) = (app.tl.peaks.clone(), app.tl.seq_peak_keys.clone(), app.tl.peaks_pending.clone(), app.tl.peak_passes.clone());
    let project = app.session.project.clone();
    let provider = app.session.media.provider(project.clone(), app.session.services.clone());
    let run = move || {
        let mut pass = passes.lock().unwrap_or_else(|e| e.into_inner()).remove(&pkey).filter(|p| p.generation == key).unwrap_or_else(|| PeakPass::new(key));
        // a panic in the mix must not take the app down with it, nor leave the item pending for good
        let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let seq = project.sequence(item)?;
            let sr = seq.settings.sample_rate.max(1);
            let total = seq.duration().to_units_floor(sr as i64).max(0) as usize;
            // one peak per 256 samples at 48 kHz, whatever the sequence's own rate
            let bucket = ((256u64 * sr as u64) / 48_000).max(1) as usize;
            let chunk = (bucket * 750).max(1);
            Some(pass.run(total, chunk, |s, n| {
                let buf = filmcraft_render::audio::mix_sequence(&project, seq, s as i64, n, &provider);
                Some(buf.channels.first()?.chunks(bucket).map(min_max).collect())
            }))
        }));
        match mixed {
            Ok(Some(true)) => {
                peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(std::mem::take(&mut pass.out)));
                keys.lock().unwrap_or_else(|e| e.into_inner()).insert(item, key);
            }
            // a read had to wait (web): go on from here on a later frame
            Ok(Some(false)) => {
                passes.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, pass);
            }
            _ => {}
        }
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != pkey);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    have
}

/// Min/max peaks (one per 256 samples at 48 kHz) of audio stream `stream` of `src`, `dur` long,
/// read on from where `pass` stopped. True when finished (see [`PeakPass::run`]).
fn source_pass(src: &dyn filmcraft_media::MediaSource, stream: usize, dur: Tick, pass: &mut PeakPass) -> bool {
    let sr = 48_000u32;
    let total = dur.to_units_floor(sr as i64).max(0) as usize;
    pass.run(total, 48_000 * 4, |s, n| {
        let buf = src.audio_stream(stream, s as i64, n, sr).ok()?;
        Some(buf.channels.first().map(|ch| ch.chunks(256).map(min_max).collect()).unwrap_or_default())
    })
}

/// Min/max peaks (one per 256 samples at 48 kHz) of audio stream `stream` of `src`, `dur` long.
#[cfg(test)]
pub(super) fn source_peaks(src: &dyn filmcraft_media::MediaSource, stream: usize, dur: Tick) -> Vec<(f32, f32)> {
    let mut pass = PeakPass::default();
    source_pass(src, stream, dur, &mut pass);
    pass.out
}

/// Whether the pool stands in the offline slate (silence) for media item `media`'s file: missing
/// or made offline. An unreadable file may still have readable audio.
fn shows_slate(pool: &filmcraft_engine::MediaPool, media: ItemId) -> bool {
    use filmcraft_render::offline::OfflineReason;
    pool.offline_status(media).is_some_and(|o| matches!(o.reason, OfflineReason::Missing | OfflineReason::MadeOffline))
}

/// Waveform peaks of audio stream `stream` of `item` (a clip's `audio_stream`). An offline item
/// has none (its slate is silence; cached peaks stay hidden while it is offline), and an item
/// whose file came back ([`filmcraft_engine::MediaPool::reopen`]) gets new ones. Offline state and
/// reopen count are those of the media the item plays (a subclip's parent). A pass that had to
/// wait for bytes (web) resumes where it stopped ([`PeakPass`]).
pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId, stream: usize) -> Option<Arc<Vec<(f32, f32)>>> {
    if app.session.project.sequence(item).is_some() {
        return request_sequence_peaks(app, item);
    }
    let pkey = (item, stream);
    let (media, made_offline) = app.session.project.resolve_media(item).map(|(id, m, _)| (id, m.offline))?;
    // No source: try again on a later frame instead of leaving the item pending forever.
    let earlier = filmcraft_media::pending::take();
    let src = app.session.source(item);
    let deferred = filmcraft_media::pending::take();
    if earlier || deferred {
        filmcraft_media::pending::mark();
    }
    let src = src?;
    if made_offline || deferred || shows_slate(&app.session.media, media) {
        return None;
    }
    let generation = app.session.media.reopened(media);
    {
        let mut cached = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cached.get(&pkey) {
            if app.tl.peak_gens.lock().unwrap_or_else(|e| e.into_inner()).get(&pkey) == Some(&generation) {
                return Some(p.clone());
            }
            cached.remove(&pkey);
        }
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return None;
        }
        let parts = app.tl.peak_passes.lock().unwrap_or_else(|e| e.into_inner());
        if parts.get(&pkey).is_some_and(|p| p.generation == generation && p.waiting()) {
            return None;
        }
        pend.push(pkey);
    }
    let (peaks, gens, pending, passes) = (app.tl.peaks.clone(), app.tl.peak_gens.clone(), app.tl.peaks_pending.clone(), app.tl.peak_passes.clone());
    let pool = app.session.media.clone();
    let dur = src.info().duration;
    let run = move || {
        let mut pass =
            passes.lock().unwrap_or_else(|e| e.into_inner()).remove(&pkey).filter(|p| p.generation == generation).unwrap_or_else(|| PeakPass::new(generation));
        let done = source_pass(&*src, stream, dur, &mut pass);
        // not this file's waveform: a source that went offline meanwhile, or a file that came back
        // while these were read
        if !shows_slate(&pool, media) && pool.reopened(media) == generation {
            if done {
                peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(pass.out));
                gens.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, generation);
            } else {
                passes.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, pass);
            }
        }
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != pkey);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;
    use serde_json::json;

    /// 2 s of a 440 Hz tone, PCM16 mono 48 kHz.
    fn tone_wav() -> Vec<u8> {
        let (rate, samples) = (48_000u32, 96_000u32);
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(samples * 2).to_le_bytes());
        for i in 0..samples {
            let v = (12_000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin()) as i16;
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    fn loudest(p: &[(f32, f32)]) -> f32 {
        p.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()))
    }

    /// Peaks of `item`, waiting for the background job (None when none come).
    fn peaks_of(app: &mut FilmcraftApp, item: ItemId) -> Option<Arc<Vec<(f32, f32)>>> {
        for _ in 0..200 {
            if let Some(p) = request_peaks(app, item, 0) {
                return Some(p);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    // T3 (Filmsquare): a quiet-opened project gets its audio afterwards (`relink::attach`). The
    // waveform drawn while the file was missing (the slate is silence) must not stay: the clip's
    // peaks after attach are the file's, and so are the nested sequence's.
    #[test]
    fn waveforms_after_attach_are_the_files_not_the_silence() {
        let root = std::env::temp_dir().join(format!("fc-waveform-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Media")).unwrap();
        let wav = root.join("Media").join("tone.wav");
        std::fs::write(&wav, tone_wav()).unwrap();
        let wav_path = wav.to_string_lossy().into_owned();
        let mut s = Session::default();
        let r = s.execute("file.import", json!({"paths": [wav_path]})).unwrap();
        let item = ItemId(r["items"][0].as_u64().unwrap());
        let sub = ItemId(s.execute("clip.makeSubclip", json!({"item": item.0, "startFrame": 0, "endFrame": 24})).unwrap()["item"].as_u64().unwrap());
        let seq = ItemId(s.execute("file.newSequence", json!({"name": "Cut", "fromItem": item.0})).unwrap()["sequence"].as_u64().unwrap());
        let path = root.join("p.fcproj").to_string_lossy().into_owned();
        s.execute("file.save", json!({"path": path})).unwrap();
        std::fs::rename(root.join("Media"), root.join("Stash")).unwrap();

        let mut app = FilmcraftApp::new(Session::default());
        app.open_project_quietly(&path).unwrap();
        assert!(peaks_of(&mut app, item).is_none(), "a missing file has no waveform");
        assert!(app.tl.peaks.lock().unwrap().get(&(item, 0)).is_none(), "nor silent peaks kept for it");
        assert!(peaks_of(&mut app, sub).is_none(), "nor for a subclip of it");
        assert!(app.tl.peaks.lock().unwrap().get(&(sub, 0)).is_none());
        let silent = peaks_of(&mut app, seq).expect("the nested sequence mixes (silence)");
        assert!(loudest(&silent) < 1e-4);

        std::fs::rename(root.join("Stash"), root.join("Media")).unwrap();
        assert_eq!(filmcraft_engine::relink::attach(&mut app.session, &[wav_path]), vec![item]);
        let clip = peaks_of(&mut app, item).expect("the clip's waveform");
        assert!(loudest(&clip) > 0.3, "the file's peaks: {}", loudest(&clip));
        let subclip = peaks_of(&mut app, sub).expect("the subclip's waveform");
        assert!(loudest(&subclip) > 0.3, "the subclip follows its media: {}", loudest(&subclip));
        let mut mixed = 0.0;
        for _ in 0..200 {
            mixed = request_peaks(&mut app, seq, 0).map_or(0.0, |p| loudest(&p));
            if mixed > 0.3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(mixed > 0.3, "the nested sequence is mixed again with the file: {mixed}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project holding `tone.wav` (online) in `root`; returns (session, item, path).
    fn online_wav(root: &std::path::Path) -> (Session, ItemId, String) {
        let _ = std::fs::remove_dir_all(root);
        std::fs::create_dir_all(root).unwrap();
        let wav = root.join("tone.wav");
        std::fs::write(&wav, tone_wav()).unwrap();
        let path = wav.to_string_lossy().into_owned();
        let mut s = Session::default();
        let r = s.execute("file.import", json!({"paths": [path]})).unwrap();
        (s, ItemId(r["items"][0].as_u64().unwrap()), path)
    }

    // Make Offline hides a cached waveform (an offline item has none); linking the file again
    // shows it again.
    #[test]
    fn a_clip_made_offline_shows_no_waveform_until_it_is_back() {
        let root = std::env::temp_dir().join(format!("fc-waveform-offline-{}", std::process::id()));
        let (s, item, path) = online_wav(&root);
        let mut app = FilmcraftApp::new(s);
        assert!(loudest(&peaks_of(&mut app, item).expect("peaks")) > 0.3);
        app.session.execute("media.makeOffline", json!({"items": [item.0]})).unwrap();
        assert!(request_peaks(&mut app, item, 0).is_none(), "made offline: no waveform");
        app.session.execute("media.relink", json!({"item": item.0, "path": path})).unwrap();
        assert!(loudest(&peaks_of(&mut app, item).expect("back online: the waveform again")) > 0.3);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A source whose reads wait (web: bytes still being fetched) as `defer` says, counting reads.
    struct Deferring {
        info: filmcraft_media::MediaInfo,
        reads: Arc<std::sync::Mutex<Vec<i64>>>,
        defer: fn(&[i64], i64) -> bool,
    }

    impl filmcraft_media::MediaSource for Deferring {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.info
        }
        fn video_frame(&self, _req: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            Err(filmcraft_media::MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            self.audio_stream(0, start, frames, sample_rate)
        }
        fn audio_stream(&self, _stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            let mut reads = self.reads.lock().unwrap();
            let wait = (self.defer)(&reads, start);
            reads.push(start);
            if wait {
                return Err(filmcraft_media::MediaError::Io(filmcraft_media::pending::would_block().to_string()));
            }
            Ok(filmcraft_frame::AudioBuffer { sample_rate, channels: vec![vec![0.5; frames]] })
        }
    }

    /// `item` of a fresh project, its source replaced by a [`Deferring`] one `seconds` long.
    fn deferring(seconds: i64, defer: fn(&[i64], i64) -> bool) -> (FilmcraftApp, ItemId, Arc<std::sync::Mutex<Vec<i64>>>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fc-waveform-defer-{}-{n}", std::process::id()));
        let (s, item, _) = online_wav(&root);
        let reads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let info = filmcraft_media::MediaInfo {
            name: "long.mkv".into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: Tick(filmcraft_time::TICKS_PER_SECOND * seconds),
            video: None,
            audio_streams: Vec::new(),
            container: "mkv".into(),
            start_timecode: None,
            file_size: None,
        };
        s.media.insert_keyed(item, String::new(), Arc::new(Deferring { info, reads: reads.clone(), defer }));
        let _ = std::fs::remove_dir_all(&root);
        (FilmcraftApp::new(s), item, reads)
    }

    /// Ask for `item`'s peaks every few milliseconds (frames) for up to `ms`.
    fn poll(app: &mut FilmcraftApp, item: ItemId, ms: u64) -> Option<Arc<Vec<(f32, f32)>>> {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < end {
            if let Some(p) = request_peaks(app, item, 0) {
                return Some(p);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }

    // A file whose bytes never come (every read waits) costs a bounded number of reads, spaced
    // out, and then the peaks read so far (none) are kept: it is not read again every frame.
    #[test]
    fn waiting_reads_are_retried_a_bounded_number_of_times() {
        let (mut app, item, reads) = deferring(60, |_, _| true);
        let peaks = poll(&mut app, item, 3000).expect("the peaks so far are kept in the end");
        assert!(peaks.is_empty());
        assert_eq!(reads.lock().unwrap().len(), MAX_STALLS as usize, "one read per pass, {MAX_STALLS} passes");
        for _ in 0..20 {
            assert!(request_peaks(&mut app, item, 0).is_some());
        }
        assert_eq!(reads.lock().unwrap().len(), MAX_STALLS as usize, "and then never again");
    }

    // A file larger than the browser keeps (each chunk's bytes arrive only after a read asked for
    // them) is read once through: a pass that had to wait resumes at its chunk instead of starting
    // over, so no chunk is read again once it was read.
    #[test]
    fn a_pass_that_had_to_wait_resumes_where_it_stopped() {
        // every chunk waits the first time it is asked for
        let (mut app, item, reads) = deferring(60, |seen, start| !seen.contains(&start));
        let peaks = poll(&mut app, item, 10_000).expect("the whole waveform");
        assert_eq!(peaks.len(), 60 * 48_000 / 256, "all of it");
        assert!(loudest(&peaks) > 0.4);
        let reads = reads.lock().unwrap();
        let chunks = 15; // 60 s in 4 s chunks
        assert_eq!(reads.len(), 2 * chunks, "each chunk: one read that waited, one that read it");
        for start in reads.iter() {
            assert!(reads.iter().filter(|s| *s == start).count() <= 2, "chunk at {start} read again");
        }
    }
}
