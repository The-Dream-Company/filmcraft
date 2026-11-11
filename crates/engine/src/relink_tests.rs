//! Offline media and relinking end to end: moved media opens offline (the slate renders),
//! relinking one file finds the others by folder remap, fingerprint mismatches are refused,
//! Make Offline, folder search and undo.

use filmcraft_media::DemoScene;
use filmcraft_project::ItemId;
use filmcraft_render::offline::{OfflineReason, slate_rgba8};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};
use crate::relink::{apply_remap, derive_remap};

const W: u32 = 160;
const H: u32 = 90;

/// A saved project at `<root>/Project/p.fcproj` using `<root>/Media/{a,b}.mov`.
fn saved_project(root: &std::path::Path) -> (String, Vec<ItemId>) {
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let (a, b) = (media.join("a.mov"), media.join("b.mov"));
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    make_movie(&b, DemoScene::CityNight, W, H, 12);
    let (mut s, items, _) = session_with(&[&a, &b]);
    let proj = root.join("Project");
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    (path, items)
}

fn open(path: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path})).unwrap();
    s
}

#[test]
fn identity_and_remap_helpers() {
    let a = crate::relink::identity_of_bytes(&vec![7u8; 3 << 20]);
    let mut v = vec![7u8; 3 << 20];
    v[(3 << 20) - 5] = 1;
    let b = crate::relink::identity_of_bytes(&v);
    assert_eq!(a.size, b.size);
    assert_ne!(a.fingerprint, b.fingerprint, "a change in the last MiB changes the fingerprint");
    assert_eq!(derive_remap("/Volumes/A/shoot/day1/a.mov", "/Users/me/shoot/day1/a.mov"), Some(("/Volumes/A".into(), "/Users/me".into())));
    assert_eq!(derive_remap(r"D:\proj\media\a.mov", "/Volumes/D/proj/media/a.mov"), Some(("D:".into(), "/Volumes/D".into())));
    assert_eq!(apply_remap(r"D:\proj\media\b.mov", "D:", "/Volumes/D").as_deref(), Some("/Volumes/D/proj/media/b.mov"));
    assert_eq!(apply_remap("/Volumes/AB/x.mov", "/Volumes/A", "/x"), None, "prefix must end at a separator");
}

#[test]
fn moved_media_opens_offline_and_relinks_others_by_folder() {
    let root = tmp_dir("relink-moved");
    let (path, items) = saved_project(&root);
    let online = frame_rgba(&mut open(&path), 3, 1.0).2;
    // move the media folder
    let moved = root.join("Moved").join("Media");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(root.join("Media"), &moved).unwrap();

    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(r["missingMedia"], json!(2), "{r}");
    assert!(s.offline.prompt, "the Link Media dialog is requested");
    let (w, h, px) = frame_rgba(&mut s, 3, 1.0);
    let slate = slate_rgba8(w, h, "a.mov", OfflineReason::Missing);
    assert!(psnr(&px, &slate) > 40.0, "offline clip renders the offline slate");
    let m = s.execute("media.findMissing", json!({})).unwrap();
    assert_eq!(m["missing"].as_array().unwrap().len(), 2);

    // link a; b follows by the derived folder remap
    let r = s.execute("media.relink", json!({"item": items[0].0, "path": moved.join("a.mov").to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["remaining"], json!([]));
    assert!(!s.offline.prompt);
    assert_eq!(frame_rgba(&mut s, 3, 1.0).2, online, "relinked media renders as before the move");
    let b_path = s.project.item(items[1]).unwrap().as_media().unwrap().media.clone();
    let filmcraft_project::MediaRef::File { path: b_path } = b_path else { panic!("relinked media must remain file-based") };
    assert_eq!(std::path::Path::new(&b_path), moved.join("b.mov"));
    // undo brings the old (missing) paths back, and the slate with them
    s.execute("edit.undo", json!({})).unwrap();
    assert!(psnr(&frame_rgba(&mut s, 3, 1.0).2, &slate) > 40.0);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(frame_rgba(&mut s, 3, 1.0).2, online);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn auto_relink_by_prefix_and_by_folder_search() {
    let root = tmp_dir("relink-auto");
    let (path, items) = saved_project(&root);
    let dest = root.join("Archive").join("2026");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::rename(root.join("Media"), dest.join("Media")).unwrap();
    let mut s = open(&path);
    let r = s.execute("media.autoRelink", json!({"from": root.join("Media").to_string_lossy(), "to": dest.join("Media").to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    assert!(s.execute("media.findMissing", json!({})).unwrap()["missing"].as_array().unwrap().is_empty());

    // again, this time searching a folder tree
    let mut s = open(&path);
    let r = s.execute("media.search", json!({"folder": root.to_string_lossy(), "item": items[1].0})).unwrap();
    let c = &r["results"][0]["candidates"][0];
    assert_eq!(c["identityMatch"], json!(true), "{r}");
    let r = s.execute("media.autoRelink", json!({"folder": root.to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fingerprint_mismatch_is_refused_unless_forced() {
    let root = tmp_dir("relink-mismatch");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    // same name, duration, size and rate — different pictures
    let other = root.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    make_movie(&other.join("a.mov"), DemoScene::Dunes, W, H, 12);
    let mut s = open(&path);
    let e = s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("not the same file"), "{e}");
    assert_eq!(s.offline.missing.len(), 2, "nothing changed");
    let r =
        s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy(), "force": true, "relinkOthers": false})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 1);
    assert_eq!(r["remaining"], json!([items[1].0]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn forced_relink_takes_the_new_files_properties() {
    let root = tmp_dir("relink-forced-info");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    // same name, different pictures and a different size
    let other = root.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    make_movie(&other.join("a.mov"), DemoScene::Dunes, W * 2, H * 2, 12);
    let mut s = open(&path);
    s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy(), "force": true, "relinkOthers": false})).unwrap();
    let width = s.project.item(items[0]).and_then(|i| i.as_media()).and_then(|m| m.info.video.as_ref().map(|v| v.width));
    assert_eq!(width, Some(W * 2), "the item describes the replacement file");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn make_offline_shows_the_slate_until_relinked() {
    let root = tmp_dir("relink-offline");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let online = frame_rgba(&mut s, 3, 1.0);
    s.execute("project.select", json!({"items": [items[0].0]})).unwrap();
    s.execute("media.makeOffline", json!({})).unwrap();
    let (w, h, px) = frame_rgba(&mut s, 3, 1.0);
    assert!(psnr(&px, &slate_rgba8(w, h, "a.mov", OfflineReason::MadeOffline)) > 40.0);
    let st = s.execute("media.status", json!({"item": items[0].0})).unwrap();
    assert_eq!(st[0]["status"], json!("offline"));
    assert!(root.join("Media/a.mov").exists(), "media stays on disk by default");
    let a = root.join("Media/a.mov").to_string_lossy().into_owned();
    s.execute("media.relink", json!({"item": items[0].0, "path": a})).unwrap();
    assert_eq!(frame_rgba(&mut s, 3, 1.0), online);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn relink_search_does_not_follow_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tmp_dir("relink-no-symlink-walk");
    let inside = root.join("inside");
    let outside = root.join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(inside.join("kept.mov"), b"kept").unwrap();
    std::fs::write(outside.join("outside.mov"), b"outside").unwrap();
    symlink(&outside, inside.join("loop")).unwrap();
    symlink(outside.join("outside.mov"), inside.join("linked.mov")).unwrap();

    let index = crate::relink::index_folder(&inside, 20_000);
    assert!(index.contains_key("kept.mov"));
    assert!(!index.contains_key("outside.mov"));
    // a symlinked file is still media the user can relink to
    assert!(index.contains_key("linked.mov"));
    let _ = std::fs::remove_dir_all(&root);
}

// Regression test for issue #110: after `file.import` re-imports a file at a path the project
// already listed as offline, `s.offline.missing` must drop that item so the slate and the
// "Media missing" badge clear without reopening the project. Before the fix (the patch in
// `file.import` that calls `crate::relink::refresh(s)`), the slate stayed stale until reopen
// or per-item relink.
#[test]
fn reimport_clears_offline_list_after_path_resolves() {
    let root = tmp_dir("relink-reimport");
    let (path, items) = saved_project(&root);
    let a = root.join("Media/a.mov");

    // Delete a.mov so the saved project now points at a missing file, then reopen:
    // `on_open` → `refresh` → `scan` registers items[0] as missing.
    let online = frame_rgba(&mut open(&path), 3, 1.0);
    std::fs::remove_file(&a).unwrap();
    let mut s = open(&path);
    assert!(s.offline.missing.contains(&items[0]), "setup: a.mov missing on reopen");
    assert_ne!(frame_rgba(&mut s, 3, 1.0), online, "setup: the slate shows while a.mov is missing");

    // Recreate the file at the exact path the project stores, then import. `file.import`
    // must drop items[0] from `s.offline.missing`. Without the fix, the entry stays stale
    // and the user has to reopen or `media.relink` each item.
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    s.execute("file.import", json!({"paths": [a.to_string_lossy()]})).unwrap();
    assert!(!s.offline.missing.contains(&items[0]), "issue #110 — file.import did not refresh s.offline.missing (still contains {:?})", s.offline.missing);
    assert_eq!(s.execute("media.findMissing", json!({})).unwrap()["missing"].as_array().unwrap().len(), 0);
    // and the monitors show the file again, not the cached slate
    assert_eq!(frame_rgba(&mut s, 3, 1.0), online);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn explicit_folder_remaps_normalize_both_prefixes_and_respect_boundaries() {
    assert_eq!(apply_remap(r"C:\shoot\Media\a.mov", r"C:\shoot\Media\", r"D:\archive\Media\"), Some("D:/archive/Media/a.mov".into()));
    assert_eq!(apply_remap("C:/shoot/Media/a.mov", r"C:\shoot\Media", r"D:\archive\Media"), Some("D:/archive/Media/a.mov".into()));
    assert_eq!(apply_remap("C:/shoot/Media2/a.mov", r"C:\shoot\Media", r"D:\archive\Media"), None);
}

/// #1054: an export (or a rendered frame) that draws the offline slate names the offline items it
/// used, with their status, so a headless caller can tell placeholder frames from real ones. Media
/// outside the exported range is not reported.
#[test]
fn export_names_the_offline_media_it_rendered() {
    let root = tmp_dir("relink-export-offline");
    let (path, items) = saved_project(&root);
    let out = |name: &str| root.join(name).to_string_lossy().into_owned();
    let mut s = open(&path);
    let r = s
        .execute("file.exportMedia", json!({"path": out("on.png"), "format": "png", "range": "custom", "startSeconds": 0, "endSeconds": 0.05, "wait": true}))
        .unwrap();
    assert_eq!(r["offlineMedia"], json!([]), "{r}");
    std::fs::remove_file(root.join("Media/a.mov")).unwrap();
    let mut s = open(&path);
    // b.mov (frames 12..24) is still there
    let r = s
        .execute("file.exportMedia", json!({"path": out("b.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "wait": true}))
        .unwrap();
    assert_eq!(r["offlineMedia"], json!([]), "{r}");
    let r = s.execute("file.exportMedia", json!({"path": out("all.wav"), "format": "wav", "wait": true})).unwrap();
    let off = r["offlineMedia"].as_array().unwrap();
    assert_eq!(off.len(), 1, "{r}");
    assert_eq!(off[0]["item"], json!(items[0].0));
    assert_eq!(off[0]["name"], json!("a.mov"));
    assert_eq!(off[0]["status"], json!("missing"));
    assert!(off[0]["path"].as_str().unwrap().ends_with("a.mov"), "{r}");
    // made offline on purpose
    s.execute("media.makeOffline", json!({"items": [items[1].0]})).unwrap();
    let r = s
        .execute("file.exportMedia", json!({"path": out("b2.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "wait": true}))
        .unwrap();
    assert_eq!(
        r["offlineMedia"].as_array().map(|a| a.iter().map(|o| (o["item"].clone(), o["status"].clone())).collect::<Vec<_>>()),
        Some(vec![(json!(items[1].0), json!("offline"))]),
        "{r}"
    );
    // the queue reports it per item
    let r = s
        .execute(
            "export.queue.add",
            json!({"path": out("q.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "start": true, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["items"][0]["offlineMedia"][0]["status"], json!("offline"), "{r}");
    // one rendered frame
    let (_, off) = s.try_render_program_with_offline_at(0.5, s.sequence_rate().tick_of(3)).unwrap();
    assert_eq!(off.iter().map(|o| (o.item, o.status)).collect::<Vec<_>>(), [(items[0].0, "missing")]);
    let (_, off) = s.try_render_program_with_offline_at(0.5, s.sequence_rate().tick_of(30)).unwrap();
    assert!(off.is_empty(), "nothing on the timeline there: {off:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// #1054: `jobs.list` carries an export job's `offlineMedia`, for callers that poll the job (the
/// bridge and MCP long exports) instead of waiting in `file.exportMedia`.
#[test]
fn jobs_list_reports_an_exports_offline_media() {
    let root = tmp_dir("relink-jobs-offline");
    let (path, items) = saved_project(&root);
    std::fs::remove_file(root.join("Media/b.mov")).unwrap();
    let mut s = open(&path);
    let out = root.join("all.wav").to_string_lossy().into_owned();
    let job = s.execute("file.exportMedia", json!({"path": out, "format": "wav", "wait": true})).unwrap()["job"].clone();
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap();
    assert_eq!(j["offlineMedia"][0]["item"], json!(items[1].0), "{j}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deleting_bin_keeps_subclip_source_but_removes_unused_media() {
    let root = tmp_dir("delete-bin-used-media");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let p = std::sync::Arc::make_mut(&mut s.project);
    let seq = s.state.active_sequence.unwrap();
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let sub =
        p.add_item("Subclip", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    for track in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        track.items.retain(|clip| clip.item != items[1]);
        for clip in &mut track.items {
            clip.item = sub;
        }
    }
    let bin = p.add_bin("Sources", None);
    p.move_to_bin(&items, Some(bin)).unwrap();
    let before = s.active_sequence().unwrap().clone();
    s.execute("project.delete", json!({"items": [bin.0, items[0].0, bin.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before);
    assert!(s.project.root.find_bin(bin).is_none());
    assert!(s.project.item(items[0]).unwrap().as_media().unwrap().offline);
    assert!(s.project.item(items[1]).is_none());
    assert!(s.project.resolve_media(sub).unwrap().1.offline);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(items[0]), Some(bin));
    assert!(s.project.item(items[1]).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_sequence_and_its_media_does_not_keep_an_unused_source() {
    let root = tmp_dir("delete-sequence-and-media");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    s.execute("project.delete", json!({"items": [seq.0, items[0].0]})).unwrap();
    assert!(s.project.item(seq).is_none());
    assert!(s.project.item(items[0]).is_none());
    assert!(s.project.item(items[1]).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_subclip_and_source_does_not_keep_an_unused_source() {
    let root = tmp_dir("delete-subclip-and-source");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let sub =
        p.add_item("Subclip", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    for t in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        for c in &mut t.items {
            if c.item == items[0] {
                c.item = sub;
            }
        }
    }
    s.execute("project.delete", json!({"items": [sub.0, items[0].0]})).unwrap();
    assert!(s.project.item(sub).is_none());
    assert!(s.project.item(items[0]).is_none(), "a removed timeline subclip must not retain its unused source");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_bin_preserves_intermediate_subclip_needed_by_surviving_clip() {
    let root = tmp_dir("delete-intermediate-subclip");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let parent =
        p.add_item("Parent", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    let outer = p.add_item("Outer", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent, range, restrict_trims: false }, None);
    for t in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        for c in &mut t.items {
            if c.item == items[0] {
                c.item = outer;
            }
        }
    }
    let bin = p.add_bin("Sources", None);
    p.move_to_bin(&[items[0], parent], Some(bin)).unwrap();
    let before = s.active_sequence().unwrap().clone();
    s.execute("project.delete", json!({"items": [bin.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before);
    assert!(s.project.resolve_media(outer).is_some(), "the surviving nested subclip must still resolve");
    assert!(s.project.resolve_media(outer).unwrap().1.offline);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(parent), Some(bin));
    let _ = std::fs::remove_dir_all(root);
}

/// Point item `id` at `path` the way a project file from someone else could.
fn set_media_path(s: &mut Session, id: ItemId, path: &str) {
    let p = std::sync::Arc::make_mut(&mut s.project);
    let m = p.item_mut(id).and_then(|i| i.as_media_mut()).unwrap();
    m.media = filmcraft_project::MediaRef::File { path: path.to_string() };
}

// #2782: Make Offline ▸ delete removed whatever file an item's path named.
#[test]
fn make_offline_delete_only_removes_the_media_the_item_was_linked_to() {
    let root = tmp_dir("relink-delete-guard");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let notes = root.join("notes.txt");
    std::fs::write(&notes, b"keep me").unwrap();
    let other = root.join("other.mov");
    make_movie(&other, DemoScene::CityNight, W, H, 12);
    let dir = root.join("folder.mov");
    std::fs::create_dir_all(&dir).unwrap();

    // not media, a different take under the same kind of name, and a folder: all survive
    for victim in [&notes, &other, &dir] {
        set_media_path(&mut s, items[0], &victim.to_string_lossy());
        let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
        assert_eq!(r["deleted"], json!([]), "{victim:?}");
        assert!(victim.exists(), "{victim:?} was deleted");
    }
    // an item with no recorded identity (a placeholder, an older project) never deletes anything
    set_media_path(&mut s, items[0], &a.to_string_lossy());
    std::sync::Arc::make_mut(&mut s.project).item_mut(items[0]).and_then(|i| i.as_media_mut()).unwrap().identity = None;
    let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
    assert_eq!(r["deleted"], json!([]), "{r}");
    assert!(a.exists(), "a file with no recorded identity was deleted");
    // the file the item was imported from is deleted
    let id_a = crate::relink::identity_of(&*s.services, &a.to_string_lossy()).unwrap();
    std::sync::Arc::make_mut(&mut s.project).item_mut(items[0]).and_then(|i| i.as_media_mut()).unwrap().identity = Some(id_a);
    set_media_path(&mut s, items[0], &a.to_string_lossy());
    let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
    assert_eq!(r["deleted"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!a.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_offline_file_keeps_only_the_file_name() {
    let mut s = Session::default();
    for (given, want) in [
        ("/etc/passwd", "passwd"),
        ("..\\..\\x\\y.mov", "y.mov"),
        ("clip.mov", "clip.mov"),
        ("C:target.mov", "target.mov"),
        ("C:", "Offline.mov"),
        ("/", "Offline.mov"),
        ("..", "Offline.mov"),
    ] {
        let r = s.execute("file.newOfflineFile", json!({"fileName": given})).unwrap();
        let id = ItemId(r["item"].as_u64().unwrap());
        let m = s.project.item(id).unwrap().as_media().unwrap();
        assert_eq!(m.media, filmcraft_project::MediaRef::File { path: want.to_string() }, "{given}");
    }
}

// Offline All must be saved with the project: reopening it should not ask for the files again
// (Premiere behaves this way). It used to only close the dialog, so every open asked again.
#[test]
fn offline_all_is_saved_and_reopening_does_not_ask_again() {
    let root = tmp_dir("relink-offline-all");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    let mut s = open(&path);
    assert!(s.offline.prompt, "setup: missing media asks");
    s.execute("media.offlineAll", json!({})).unwrap();
    assert!(!s.offline.prompt);
    assert!(items.iter().all(|i| s.project.item(*i).unwrap().as_media().unwrap().offline));
    s.execute("file.save", json!({})).unwrap();

    let s = open(&path);
    assert!(!s.offline.prompt, "reopening after Offline All must not show Link Media again");
    assert_eq!(s.offline.missing.len(), 2, "File ▸ Link Media… still lists them");

    // media that goes missing later still asks on the next open
    let mut s = open(&path);
    s.edit("relink b", |p, _| {
        let m = p.item_mut(items[1]).unwrap().as_media_mut().unwrap();
        m.offline = false;
        Ok(())
    })
    .unwrap();
    s.execute("file.save", json!({})).unwrap();
    assert!(open(&path).offline.prompt, "newly missing media asks again");
    let _ = std::fs::remove_dir_all(&root);
}

/// `<root>/Project/p.fcproj` using `<root>/Media/{a,b,c}.mov`, then the media folder moved to
/// `<root>/Stash`: the project opens with three missing items.
fn three_missing(root: &std::path::Path) -> (String, Vec<ItemId>, Vec<String>) {
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let files = [media.join("a.mov"), media.join("b.mov"), media.join("c.mov")];
    for (f, scene) in files.iter().zip([DemoScene::Aurora, DemoScene::CityNight, DemoScene::Dunes]) {
        make_movie(f, scene, W, H, 12);
    }
    let (mut s, items, _) = session_with(&[&files[0], &files[1], &files[2]]);
    let proj = root.join("Project");
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    drop(s);
    std::fs::rename(&media, root.join("Stash")).unwrap();
    (path, items, files.iter().map(|f| f.to_string_lossy().into_owned()).collect())
}

/// What a host does after `attachFiles`: put the file back at the path the project names.
fn put_back(root: &std::path::Path, name: &str) {
    std::fs::create_dir_all(root.join("Media")).unwrap();
    std::fs::rename(root.join("Stash").join(name), root.join("Media").join(name)).unwrap();
}

fn status(s: &mut Session) -> Vec<String> {
    s.execute("media.status", json!({})).unwrap().as_array().unwrap().iter().map(|m| m["status"].as_str().unwrap().to_string()).collect()
}

// T3 (Filmsquare): open first, hand the media over afterwards. A file put at the path an item
// names brings that item online without an import: no bin item, no undo step.
#[test]
fn attach_brings_items_online_by_path_without_an_edit() {
    let root = tmp_dir("relink-attach");
    let (path, items, files) = three_missing(&root);
    let mut s = open(&path);
    assert_eq!(s.offline.missing.len(), 3);
    // the monitors drew the slate before the media arrived
    let (w, h, px) = frame_rgba(&mut s, 3, 1.0);
    assert!(psnr(&px, &slate_rgba8(w, h, "a.mov", OfflineReason::Missing)) > 40.0);
    // an edit before the media arrives, and where the user is
    s.edit("Rename c", |p, _| {
        p.item_mut(items[2]).unwrap().name = "c renamed".into();
        Ok(())
    })
    .unwrap();
    s.state.project_selection = vec![items[1]];
    let playhead = s.playhead();
    let (undo, item_count, dirty) = (s.history.undo.len(), s.project.items.len(), s.is_dirty());

    put_back(&root, "a.mov");
    put_back(&root, "b.mov");
    let back = crate::relink::attach(&mut s, &files[..2]);
    assert_eq!(s.playhead(), playhead, "attach leaves the playhead");
    assert_eq!(back, vec![items[0], items[1]]);
    assert_eq!(status(&mut s), ["online", "online", "missing"]);
    assert_eq!(s.offline.missing, vec![items[2]]);
    assert_eq!([0, 1, 2].map(|i| s.media.reopened(items[i])), [1, 1, 0], "frame caches of the items that came back go");
    assert!(psnr(&frame_rgba(&mut s, 3, 1.0).2, &slate_rgba8(w, h, "a.mov", OfflineReason::Missing)) < 20.0, "a renders its file now");
    let (_, _, c) = frame_rgba(&mut s, 27, 1.0);
    assert!(psnr(&c, &slate_rgba8(w, h, "c.mov", OfflineReason::Missing)) > 40.0, "c is still missing");
    // nothing imported, nothing edited, nothing moved
    assert_eq!(s.history.undo.len(), undo, "attach adds no undo step");
    assert_eq!(s.project.items.len(), item_count, "attach adds no bin item");
    assert_eq!(s.is_dirty(), dirty);
    assert_eq!(s.state.project_selection, vec![items[1]]);

    // undo takes back the edit made before attach, not the media
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.item(items[2]).unwrap().name, "c.mov");
    assert_eq!(status(&mut s), ["online", "online", "missing"]);

    // a file that matches no item is only there: no bin item, nothing comes online
    let stray = root.join("Stash").join("stray.mov");
    make_movie(&stray, DemoScene::Aurora, W, H, 4);
    let rev = s.revision;
    assert!(crate::relink::attach(&mut s, &[stray.to_string_lossy().into_owned()]).is_empty());
    assert_eq!(s.project.items.len(), item_count);
    assert_eq!(s.revision, rev, "nothing to redraw");
    let _ = std::fs::remove_dir_all(&root);
}

// The hub marks media offline on purpose in the document: attaching a file at its path leaves it
// offline (relinking clears that mark, attaching does not).
#[test]
fn attach_leaves_items_made_offline_offline() {
    let root = tmp_dir("relink-attach-offline");
    let (path, items, files) = three_missing(&root);
    let mut s = open(&path);
    s.edit("offline a", |p, _| {
        p.item_mut(items[0]).unwrap().as_media_mut().unwrap().offline = true;
        Ok(())
    })
    .unwrap();
    crate::relink::refresh(&mut s);
    put_back(&root, "a.mov");
    assert!(crate::relink::attach(&mut s, &files[..1]).is_empty());
    assert_eq!(status(&mut s)[0], "offline");
    let _ = std::fs::remove_dir_all(&root);
}

// What the host relies on: attaching to a clean project leaves the saved document (`file.saveCopy`,
// open sequences and timeline views included) byte for byte as it was, the playhead where it was,
// the project unmodified and the history as it was.
#[test]
fn attach_to_a_clean_project_changes_no_document_byte() {
    let root = tmp_dir("relink-attach-clean");
    let (path, items, files) = three_missing(&root);
    let mut s = open(&path);
    assert!(!s.is_dirty(), "setup: a clean project");
    s.set_playhead(s.sequence_rate().tick_of(5));
    let copy = root.join("copy.fcproj").to_string_lossy().into_owned();
    let save = |s: &mut Session| {
        s.execute("file.saveCopy", json!({"path": copy})).unwrap();
        std::fs::read(&copy).unwrap()
    };
    let doc = save(&mut s);
    assert!(String::from_utf8_lossy(&doc).contains("\"view\""), "setup: the copy holds the view");
    let (undo, playhead, open_sequences) = (s.history.undo.len(), s.playhead(), s.state.open_sequences.clone());
    put_back(&root, "a.mov");
    put_back(&root, "b.mov");
    assert_eq!(crate::relink::attach(&mut s, &files), vec![items[0], items[1]]);
    assert_eq!(save(&mut s), doc, "the saved document is untouched, view included");
    assert_eq!(s.playhead(), playhead);
    assert_eq!(s.state.open_sequences, open_sequences);
    assert!(!s.is_dirty(), "attach does not mark the project modified");
    assert_eq!(s.history.undo.len(), undo);
    let _ = std::fs::remove_dir_all(&root);
}

// Proxies on, the original and its proxy both missing: attaching only the proxy file brings no item
// online (the original is still missing), but the frames drawn offline must go, so the monitors
// redraw from the proxy at the same playhead. Attaching nothing new changes nothing.
#[test]
fn attaching_only_the_proxy_redraws_from_it() {
    let root = tmp_dir("relink-attach-proxy");
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let (a, px) = (media.join("a.mov"), media.join("a_proxy.mov"));
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    make_movie(&px, DemoScene::Dunes, W, H, 12);
    let px_path = px.to_string_lossy().into_owned();
    let (mut s, items, _) = session_with(&[&a]);
    let (proxy, item) = (px_path.clone(), items[0]);
    s.edit("proxy", move |p, _| {
        p.item_mut(item).unwrap().as_media_mut().unwrap().proxy = Some(filmcraft_project::MediaRef::File { path: proxy });
        Ok(())
    })
    .unwrap();
    std::fs::create_dir_all(root.join("Project")).unwrap();
    let path = root.join("Project").join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    s.media.set_use_proxies(true);
    let proxy_picture = frame_rgba(&mut s, 3, 1.0).2;
    drop(s);
    std::fs::rename(&media, root.join("Stash")).unwrap();

    let mut s = open(&path);
    s.media.set_use_proxies(true);
    let (w, h, px0) = frame_rgba(&mut s, 3, 1.0);
    assert!(psnr(&px0, &slate_rgba8(w, h, "a.mov", OfflineReason::Missing)) > 40.0, "setup: the slate");
    let (rev, playhead) = (s.revision, s.playhead());
    put_back(&root, "a_proxy.mov");
    assert!(crate::relink::attach(&mut s, std::slice::from_ref(&px_path)).is_empty(), "the original is still missing");
    assert_eq!(s.media.reopened(items[0]), 1, "frame caches of the item go");
    assert!(s.revision > rev, "the monitors redraw");
    assert_eq!(s.playhead(), playhead);
    let img = s.render_program(1.0).unwrap();
    assert_eq!(img.over_black_rgba8(), proxy_picture, "the frame at the same playhead comes from the proxy");
    // the same file again (the page passes no new path): nothing to redraw
    let rev = s.revision;
    assert!(crate::relink::attach(&mut s, &[]).is_empty());
    assert_eq!((s.revision, s.media.reopened(items[0])), (rev, 1));
    let _ = std::fs::remove_dir_all(&root);
}

fn missing_toasts(s: &Session) -> usize {
    s.events.iter().filter(|e| matches!(e, crate::Event::Toast { message, .. } if message.contains("media file(s) are missing"))).count()
}

// A quiet open asks nothing: no Link Media prompt and no "media file(s) are missing" toast. A
// normal open still does both, and a quiet open that fails leaves the open project as it was.
#[test]
fn quiet_open_neither_prompts_nor_toasts_and_a_failed_one_changes_nothing() {
    let root = tmp_dir("relink-quiet-open");
    let (path, _, _) = three_missing(&root);
    let s = open(&path);
    assert!(s.offline.prompt);
    assert_eq!(missing_toasts(&s), 1, "a normal open asks");

    let mut s = Session::default();
    let r = crate::relink::open_quietly(&mut s, &path).unwrap();
    assert_eq!(r["missingMedia"], json!(3), "{r}");
    assert!(!s.offline.prompt, "no Link Media");
    assert_eq!(missing_toasts(&s), 0, "no missing-media toast");
    assert_eq!(s.offline.missing.len(), 3, "File ▸ Link Media… still lists them");
    assert!(!s.offline.quiet, "only that open was quiet");

    // a normal open later asks again
    s.execute("file.open", json!({"path": path})).unwrap();
    assert!(s.offline.prompt);

    // failing: nothing changes
    let (project, prompt, rev) = (s.project.clone(), s.offline.prompt, s.revision);
    let bad = root.join("nope.fcproj").to_string_lossy().into_owned();
    assert!(crate::relink::open_quietly(&mut s, &bad).is_err());
    assert!(std::sync::Arc::ptr_eq(&s.project, &project));
    assert_eq!((s.offline.prompt, s.revision, s.offline.quiet), (prompt, rev, false));
    let _ = std::fs::remove_dir_all(&root);
}
