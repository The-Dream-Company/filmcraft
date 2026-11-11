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
        pend.push(pkey);
    }
    let (peaks, keys, pending) = (app.tl.peaks.clone(), app.tl.seq_peak_keys.clone(), app.tl.peaks_pending.clone());
    let project = app.session.project.clone();
    let provider = app.session.media.provider(project.clone(), app.session.services.clone());
    let run = move || {
        // a panic in the mix must not take the app down with it, nor leave the item pending for good
        let earlier = filmcraft_media::pending::take();
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
        // a read that had to wait (web) mixed silence in: mix again on a later frame
        let deferred = filmcraft_media::pending::take();
        if earlier || deferred {
            filmcraft_media::pending::mark();
        }
        if let Ok(Some(out)) = mixed
            && !deferred
        {
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

/// Whether the pool stands in the offline slate (silence) for `item`'s file: missing or made
/// offline. An unreadable file may still have readable audio.
fn shows_slate(pool: &filmcraft_engine::MediaPool, item: ItemId) -> bool {
    use filmcraft_render::offline::OfflineReason;
    pool.offline_status(item).is_some_and(|o| matches!(o.reason, OfflineReason::Missing | OfflineReason::MadeOffline))
}

/// Waveform peaks of audio stream `stream` of `item` (a clip's `audio_stream`). An offline item
/// has none (its slate is silence), peaks read while bytes were still loading are not kept, and
/// an item whose file came back ([`filmcraft_engine::MediaPool::reopen`]) gets new ones.
pub(crate) fn request_peaks(app: &mut FilmcraftApp, item: ItemId, stream: usize) -> Option<Arc<Vec<(f32, f32)>>> {
    if app.session.project.sequence(item).is_some() {
        return request_sequence_peaks(app, item);
    }
    let pkey = (item, stream);
    let generation = app.session.media.reopened(item);
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
    let earlier = filmcraft_media::pending::take();
    let src = app.session.source(item);
    let deferred = filmcraft_media::pending::take();
    if earlier || deferred {
        filmcraft_media::pending::mark();
    }
    let src = src?;
    if deferred || shows_slate(&app.session.media, item) {
        return None;
    }
    {
        let mut pend = app.tl.peaks_pending.lock().unwrap_or_else(|e| e.into_inner());
        if pend.contains(&pkey) {
            return None;
        }
        pend.push(pkey);
    }
    let (peaks, gens, pending) = (app.tl.peaks.clone(), app.tl.peak_gens.clone(), app.tl.peaks_pending.clone());
    let pool = app.session.media.clone();
    let dur = src.info().duration;
    let run = move || {
        let earlier = filmcraft_media::pending::take();
        let out = source_peaks(&*src, stream, dur);
        let deferred = filmcraft_media::pending::take();
        if earlier || deferred {
            filmcraft_media::pending::mark();
        }
        // not this file's waveform: a read that had to wait, a source that went offline meanwhile,
        // or a file that came back while these were read
        if !deferred && !shows_slate(&pool, item) && pool.reopened(item) == generation {
            peaks.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, Arc::new(out));
            gens.lock().unwrap_or_else(|e| e.into_inner()).insert(pkey, generation);
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
        let seq = ItemId(s.execute("file.newSequence", json!({"name": "Cut", "fromItem": item.0})).unwrap()["sequence"].as_u64().unwrap());
        let path = root.join("p.fcproj").to_string_lossy().into_owned();
        s.execute("file.save", json!({"path": path})).unwrap();
        std::fs::rename(root.join("Media"), root.join("Stash")).unwrap();

        let mut app = FilmcraftApp::new(Session::default());
        app.open_project_quietly(&path).unwrap();
        assert!(peaks_of(&mut app, item).is_none(), "a missing file has no waveform");
        assert!(app.tl.peaks.lock().unwrap().get(&(item, 0)).is_none(), "nor silent peaks kept for it");
        let silent = peaks_of(&mut app, seq).expect("the nested sequence mixes (silence)");
        assert!(loudest(&silent) < 1e-4);

        std::fs::rename(root.join("Stash"), root.join("Media")).unwrap();
        assert_eq!(filmcraft_engine::relink::attach(&mut app.session, &[wav_path]), vec![item]);
        let clip = peaks_of(&mut app, item).expect("the clip's waveform");
        assert!(loudest(&clip) > 0.3, "the file's peaks: {}", loudest(&clip));
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
}
