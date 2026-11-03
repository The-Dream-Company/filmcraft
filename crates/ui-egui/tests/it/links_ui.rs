//! Headless UI test: this build has no community links in the Help menu, the header, the About
//! dialog or the Import screen, and no Home or Import entry in the header.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `links-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("links-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn no_community_links_anywhere() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({})).to_string();
    for id in ["help.discord", "help.website", "help.appPage", "help.github", "help.reportIssue", "help.filmcraftHelp", "app.checkForUpdates"] {
        assert!(!menu.contains(id), "Help menu lists {id}");
    }
    assert!(d.ids("header.discord").is_empty());
    assert!(d.ids("header.home").is_empty());
    assert!(d.ids("header.mode.import").is_empty());
    assert_eq!(d.ids("header.mode."), vec!["header.mode.edit".to_string(), "header.mode.export".to_string()]);
    d.snapshot("header");
    d.ok("engine.execute", json!({"command": "app.about", "params": {}}));
    d.frames(3);
    assert_eq!(d.ids("about.").iter().filter(|a| !a.starts_with("about.tab.")).count(), 0);
    d.snapshot("about");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    d.ok("engine.execute", json!({"command": "mode.import", "params": {}}));
    d.frames(3);
    assert!(d.ids("import.link.").is_empty());
    d.snapshot("home");
}
