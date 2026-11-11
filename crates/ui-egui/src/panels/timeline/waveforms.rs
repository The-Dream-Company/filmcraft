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

/// What a media item's peaks are read from: the file its media (a subclip's parent) points at, and
/// how often that file came back ([`filmcraft_engine::MediaPool::reopen`]). A relink, its undo and
/// its redo change it by themselves. No allocation.
fn file_validity(project: &filmcraft_project::Project, pool: &filmcraft_engine::MediaPool, item: ItemId) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some((media, m, _)) = project.resolve_media(item) {
        pool.reopened(media).hash(&mut h);
        if let filmcraft_project::MediaRef::File { path } = &m.media {
            path.hash(&mut h);
        }
    }
    h.finish()
}

/// [`file_validity`] of every clip on sequence `item`'s audio tracks, nested sequences followed as
/// deep as `audio_segments` hashes them (8 levels).
fn sequence_files(project: &filmcraft_project::Project, pool: &filmcraft_engine::MediaPool, item: ItemId, depth: u32, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    let Some(seq) = project.sequence(item).filter(|_| depth <= 8) else { return };
    for it in seq.audio_tracks.iter().flat_map(|tr| &tr.items) {
        if project.sequence(it.item).is_some() {
            sequence_files(project, pool, it.item, depth + 1, h);
        } else {
            file_validity(project, pool, it.item).hash(h);
        }
    }
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
            // and again when one of its own files came back (`relink::attach`): it was mixed in as silence
            sequence_audio_key(&app.session.project, item).hash(&mut h);
            sequence_files(&app.session.project, &app.session.media, item, 0, &mut h);
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
        pend.push(pkey);
    }
    let (peaks, keys, pending) = (app.tl.peaks.clone(), app.tl.seq_peak_keys.clone(), app.tl.peaks_pending.clone());
    let project = app.session.project.clone();
    let provider = app.session.media.provider(project.clone(), app.session.services.clone());
    let run = move || {
        // a panic in the mix must not take the app down with it, nor leave the item pending for good
        let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let seq = project.sequence(item)?;
            let sr = seq.settings.sample_rate.max(1);
            let total = seq.duration().to_units_floor(sr as i64).max(0) as usize;
            // one peak per 256 samples at 48 kHz, whatever the sequence's own rate
            let bucket = ((256u64 * sr as u64) / 48_000).max(1) as usize;
            let chunk = (bucket * 750).max(1);
            let mut out = Vec::with_capacity(total / bucket + 1);
            let mut s = 0usize;
            while s < total {
                let n = chunk.min(total - s);
                let buf = filmcraft_render::audio::mix_sequence(&project, seq, s as i64, n, &provider);
                let ch = buf.channels.first()?;
                for c in ch.chunks(bucket) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
                s += n;
            }
            Some(out)
        }));
        if let Ok(Some(out)) = mixed {
            peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(out));
            keys.lock().unwrap_or_else(|e| e.into_inner()).insert(item, key);
        }
        pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|i| *i != pkey);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(run);
    #[cfg(target_arch = "wasm32")]
    run();
    have
}

/// Min/max peaks (one per 256 samples at 48 kHz) of audio stream `stream` of `src`, `dur` long.
pub(super) fn source_peaks(src: &dyn filmcraft_media::MediaSource, stream: usize, dur: Tick) -> Vec<(f32, f32)> {
    let sr = 48_000u32;
    let total = dur.to_units_floor(sr as i64).max(0) as usize;
    let mut out = Vec::with_capacity(total / 256 + 1);
    let chunk = 48_000 * 4;
    let mut s = 0usize;
    while s < total {
        let n = chunk.min(total - s);
        match src.audio_stream(stream, s as i64, n, sr) {
            Ok(buf) => {
                let ch = buf.channels.first().cloned().unwrap_or_default();
                for c in ch.chunks(256) {
                    let (lo, hi) = c.iter().fold((0f32, 0f32), |(l, h), v| (l.min(*v), h.max(*v)));
                    out.push((lo, hi));
                }
            }
            Err(_) => break,
        }
        s += n;
    }
    out
}

/// Waveform peaks of audio stream `stream` of `item` (a clip's `audio_stream`).
pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId, stream: usize) -> Option<Arc<Vec<(f32, f32)>>> {
    if app.session.project.sequence(item).is_some() {
        return request_sequence_peaks(app, item);
    }
    let pkey = (item, stream);
    // Peaks are of the file the item's media (a subclip's parent) pointed at when they were read: a
    // file that came back (`MediaPool::reopen`) or another file (relink, undo) gets new ones.
    let generation = file_validity(&app.session.project, &app.session.media, item);
    {
        let mut cached = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cached.get(&pkey) {
            if app.tl.peak_gens.lock().unwrap_or_else(|e| e.into_inner()).get(&pkey) == Some(&generation) {
                return Some(p.clone());
            }
            cached.remove(&pkey);
        }
    }
    // No source: try again on a later frame instead of leaving the item pending forever.
    let src = app.session.source(item)?;
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return None;
        }
        pend.push(pkey);
    }
    let (peaks, gens) = (app.tl.peaks.clone(), app.tl.peak_gens.clone());
    let pending = app.tl.peaks_pending.clone();
    let dur = src.info().duration;
    let run = move || {
        let out = source_peaks(&*src, stream, dur);
        // both under the `peaks` lock (taken first, as by the reader): never peaks without their validity
        let mut cached = peaks.lock().unwrap_or_else(|e| e.into_inner());
        cached.insert(pkey, Arc::new(out));
        gens.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, generation);
        drop(cached);
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

    /// 2 s of a 440 Hz tone at `amplitude`, PCM16 mono 48 kHz.
    fn tone_wav(amplitude: f64) -> Vec<u8> {
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
            let v = (amplitude * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin()) as i16;
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    fn loudest(p: &[(f32, f32)]) -> f32 {
        p.iter().fold(0f32, |m, (a, b)| m.max(a.abs()).max(b.abs()))
    }

    /// The loudest peak of `item` once it is `want(loudest)`, asking as the timeline does each frame.
    fn settles(app: &mut FilmcraftApp, item: ItemId, want: impl Fn(f32) -> bool) -> f32 {
        let mut last = f32::NAN;
        for _ in 0..500 {
            if let Some(p) = request_peaks(app, item, 0) {
                last = loudest(&p);
                if want(last) {
                    return last;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("peaks of {item:?} never settled (last loudest {last})");
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("fc-waveform-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // T3 (Filmsquare): a quiet-opened project gets its audio afterwards (`relink::attach`). The
    // waveform read while the file was missing (the slate is silence) must not stay: after attach
    // the clip's peaks are the file's, and so are a subclip's and the nested sequence's.
    #[test]
    fn waveforms_after_attach_are_the_files_not_the_silence() {
        let root = tmp("attach");
        std::fs::create_dir_all(root.join("Media")).unwrap();
        let wav = root.join("Media").join("tone.wav");
        std::fs::write(&wav, tone_wav(12_000.0)).unwrap();
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
        for id in [item, sub, seq] {
            assert!(settles(&mut app, id, |_| true) < 1e-4, "while missing, {id:?} reads silence");
        }

        std::fs::rename(root.join("Stash"), root.join("Media")).unwrap();
        assert_eq!(filmcraft_engine::relink::attach(&mut app.session, &[wav_path]), vec![item]);
        settles(&mut app, item, |l| l > 0.3);
        settles(&mut app, sub, |l| l > 0.3);
        settles(&mut app, seq, |l| l > 0.3);
        let _ = std::fs::remove_dir_all(&root);
    }

    // Relinking a clip to another file (Link Media, forced) shows the new file's waveform, not the
    // one read from the old file; undo and redo follow.
    #[test]
    fn a_relinked_clip_gets_the_new_files_waveform() {
        let root = tmp("relink");
        let (loud, quiet) = (root.join("loud.wav"), root.join("quiet.wav"));
        std::fs::write(&loud, tone_wav(12_000.0)).unwrap();
        std::fs::write(&quiet, tone_wav(3_000.0)).unwrap();
        let mut s = Session::default();
        let r = s.execute("file.import", json!({"paths": [loud.to_string_lossy()]})).unwrap();
        let item = ItemId(r["items"][0].as_u64().unwrap());
        let mut app = FilmcraftApp::new(s);
        assert!(settles(&mut app, item, |_| true) > 0.3, "the loud file");
        app.session.execute("media.relink", json!({"item": item.0, "path": quiet.to_string_lossy(), "force": true})).unwrap();
        let l = settles(&mut app, item, |l| l < 0.2);
        assert!(l > 0.05, "the quiet file's waveform, not silence: {l}");
        app.session.execute("edit.undo", json!({})).unwrap();
        assert!(settles(&mut app, item, |l| l > 0.3) > 0.3, "undo: the loud file's again");
        app.session.execute("edit.redo", json!({})).unwrap();
        assert!(settles(&mut app, item, |l| l < 0.2) > 0.05, "redo: the quiet file's again");
        let _ = std::fs::remove_dir_all(&root);
    }

    // A nested sequence's mix depends on its own files only: attaching a file it does not use
    // keeps its peaks (no mix again); its own file coming back mixes it again.
    #[test]
    fn a_nested_mix_follows_its_own_files_only() {
        let root = tmp("nested");
        std::fs::create_dir_all(root.join("Media")).unwrap();
        let (x, y) = (root.join("Media").join("x.wav"), root.join("Media").join("y.wav"));
        std::fs::write(&x, tone_wav(12_000.0)).unwrap();
        std::fs::write(&y, tone_wav(6_000.0)).unwrap();
        let (x_path, y_path) = (x.to_string_lossy().into_owned(), y.to_string_lossy().into_owned());
        let mut s = Session::default();
        let r = s.execute("file.import", json!({"paths": [x_path, y_path]})).unwrap();
        let x_item = ItemId(r["items"][0].as_u64().unwrap());
        let seq = ItemId(s.execute("file.newSequence", json!({"name": "X only", "fromItem": x_item.0})).unwrap()["sequence"].as_u64().unwrap());
        let path = root.join("p.fcproj").to_string_lossy().into_owned();
        s.execute("file.save", json!({"path": path})).unwrap();
        std::fs::rename(root.join("Media"), root.join("Stash")).unwrap();

        let mut app = FilmcraftApp::new(Session::default());
        app.open_project_quietly(&path).unwrap();
        assert!(settles(&mut app, seq, |_| true) < 1e-4, "X missing: silence");
        let before = request_peaks(&mut app, seq, 0).unwrap();

        std::fs::create_dir_all(root.join("Media")).unwrap();
        std::fs::rename(root.join("Stash").join("y.wav"), &y).unwrap();
        assert_eq!(filmcraft_engine::relink::attach(&mut app.session, std::slice::from_ref(&y_path)).len(), 1);
        for _ in 0..20 {
            let now = request_peaks(&mut app, seq, 0).unwrap();
            assert!(Arc::ptr_eq(&now, &before), "Y is not in the sequence: no mix again");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        std::fs::rename(root.join("Stash").join("x.wav"), &x).unwrap();
        assert_eq!(filmcraft_engine::relink::attach(&mut app.session, &[x_path]).len(), 1);
        settles(&mut app, seq, |l| l > 0.3);
        let _ = std::fs::remove_dir_all(&root);
    }
}
