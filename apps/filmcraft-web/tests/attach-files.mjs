// Host flow (Filmsquare T3): open a project whose media is missing quietly, then hand the media over
// with `filmcraft.attachFiles`. Over the Chrome DevTools Protocol, no npm dependencies (Node ≥ 22).
//
//   cargo xtask web --serve 8765 &
//   node apps/filmcraft-web/tests/attach-files.mjs --url http://127.0.0.1:8765/ [--out dir] [--headed]
//
// Gates: `filmcraftLoad.can`; a normal open still asks (Link Media, missing-media toast) and a quiet
// one does neither; attach brings the item online with no bin item, no undo step and the same
// document (`file.saveCopy`, as the host's snapshot reads it, `view` aside); a name with a path
// separator is refused; attaching a registered name again keeps the first file. Generated WAVs
// only. Prints a JSON report; exits non-zero on failure, including any Rust panic.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const arg = (k, d) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : d;
};
const base = arg("url", "http://127.0.0.1:8765/");
const out = arg("out", join(tmpdir(), "filmcraft-web-attach"));
const chrome = arg("chrome", process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "google-chrome");
const port = Number(arg("port", "9334"));
mkdirSync(out, { recursive: true });

function tone(frequency, seconds) {
  // RIFF/WAVE PCM16 mono, 48 kHz
  const rate = 48000, samples = rate * seconds;
  const b = Buffer.alloc(44 + samples * 2);
  b.write("RIFF", 0); b.writeUInt32LE(b.length - 8, 4); b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24); b.writeUInt32LE(rate * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34);
  b.write("data", 36); b.writeUInt32LE(samples * 2, 40);
  for (let i = 0; i < samples; i++) b.writeInt16LE(Math.round(12000 * Math.sin(2 * Math.PI * frequency * i / rate)), 44 + 2 * i);
  return b;
}
const media = { a: tone(440, 2), b: tone(880, 2) };
const sha = (b) => createHash("sha256").update(b).digest("hex");

const profile = mkdtempSync(join(tmpdir(), "fc-chrome-"));
const proc = spawn(chrome, [
  ...(process.argv.includes("--headed") ? [] : ["--headless=new"]),
  `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
  "--enable-unsafe-webgpu", "--window-size=1400,900", "about:blank",
], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let ws, nextId = 1;
const waiting = new Map();
const logs = [];
async function connect() {
  for (let i = 0; i < 100; i++) {
    try {
      const page = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page");
      if (page) {
        ws = new WebSocket(page.webSocketDebuggerUrl);
        await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
        ws.onmessage = (m) => {
          const msg = JSON.parse(m.data);
          if (msg.id && waiting.has(msg.id)) { waiting.get(msg.id)(msg); waiting.delete(msg.id); }
          else if (msg.method === "Runtime.consoleAPICalled") logs.push(msg.params.args.map((a) => a.value ?? a.description ?? "").join(" "));
          else if (msg.method === "Runtime.exceptionThrown") logs.push("EXCEPTION " + JSON.stringify(msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text));
        };
        return;
      }
    } catch {}
    await sleep(100);
  }
  throw new Error("Chrome did not start");
}
function send(method, params = {}) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => waiting.set(id, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result))));
}
async function js(expr) {
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, timeout: 120000 });
  if (r.exceptionDetails) throw new Error(`${expr.slice(0, 200)}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
  return r.result.value;
}
async function until(expr, ms = 120000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) { if (await js(expr)) return; await sleep(100); }
  throw new Error(`timeout waiting for ${expr}`);
}
async function load() {
  await send("Page.navigate", { url: base + "?empty&fresh" });
  await sleep(300);
  await until("!!(window.filmcraftLoad && (window.filmcraftLoad.readyMs || window.filmcraftLoad.error))");
  const l = await js("window.filmcraftLoad");
  if (l.error) throw new Error(l.error);
  await sleep(800);
  return l;
}
async function shot(name) {
  const r = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(r.data, "base64"));
}
const file = (bytes, name) => `new File([Uint8Array.from(atob(${JSON.stringify(Buffer.from(bytes).toString("base64"))}), c => c.charCodeAt(0))], ${JSON.stringify(name)})`;
const linkMedia = () => js("filmcraft.request('ui.elements', {prefix: 'linkMedia.'}).then(a => a.length)");
const missingToasts = () => js("filmcraft.execute('events.list').then(l => l.entries.filter(e => /media file\\(s\\) are missing/.test(e.message)).length)");
// the document as the host's snapshot reads it (file.saveCopy + readFile), without `view`
const snapshot = () => js(`filmcraft.execute("file.saveCopy", {path: "/projects/snapshot.fcproj"})
  .then(() => filmcraft.readFile("/projects/snapshot.fcproj"))
  .then(u => { const d = JSON.parse(new TextDecoder().decode(u)); delete d.view; return JSON.stringify(d); })`);
const readHash = (path) => js(`filmcraft.readFile(${JSON.stringify(path)}).then(u => crypto.subtle.digest("SHA-256", u)).then(h => Array.from(new Uint8Array(h), b => b.toString(16).padStart(2, "0")).join(""))`);

const report = { gates: {} };
const g = report.gates;
try {
  await connect();
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Browser.setDownloadBehavior", { behavior: "deny" }).catch(() => {});

  // the project: clip.wav at /files/clip.wav on a sequence, saved and read back
  await load();
  const imported = await js(`filmcraft.execute("file.newProject", {name: "Attach"}).then(() => filmcraft.importFiles([${file(media.a, "clip.wav")}]))`);
  if (imported.errors.length || imported.paths[0] !== "/files/clip.wav") throw new Error("import: " + JSON.stringify(imported));
  await js(`filmcraft.execute("file.newSequence", {name: "Cut", fromItem: ${imported.items[0]}})`);
  await js('filmcraft.execute("file.saveCopy", {path: "/projects/t.fcproj"})');
  const doc = await js('filmcraft.readFile("/projects/t.fcproj").then(u => { let s = ""; for (const b of u) s += String.fromCharCode(b); return btoa(s); })');
  const project = `new File([Uint8Array.from(atob(${JSON.stringify(doc)}), c => c.charCodeAt(0))], "t.fcproj")`;

  // a fresh page, normal open: Link Media and the missing-media toast, as before
  report.load = await load();
  g.can = JSON.stringify(report.load.can) === JSON.stringify(["attachFiles", "quietOpen"]);
  report.normalOpen = await js(`filmcraft.openProject(${project})`);
  await sleep(600);
  report.normalOpenAsks = { dialog: await linkMedia(), toasts: await missingToasts() };
  g.normalOpenAsks = report.normalOpen.missingMedia === 1 && report.normalOpenAsks.dialog > 0 && report.normalOpenAsks.toasts === 1;

  // a fresh page, quiet open: neither
  await load();
  report.quietOpen = await js(`filmcraft.openProject(${project}, {quiet: true})`);
  await sleep(600);
  report.quietOpenAsks = { dialog: await linkMedia(), toasts: await missingToasts() };
  g.quietOpenAsksNothing = report.quietOpen.missingMedia === 1 && report.quietOpenAsks.dialog === 0 && report.quietOpenAsks.toasts === 0;
  report.before = await js('filmcraft.execute("media.status")');
  g.missingBefore = report.before.length === 1 && report.before[0].status === "missing";
  const [docBefore, historyBefore] = [await snapshot(), await js('filmcraft.execute("history.list")')];
  await shot("1-quiet-open");

  // attach: the clip, a stray file, two names with a path separator
  report.names = await js(`[${file(media.a, "dir/x.wav")}, ${file(media.a, "dir\\y.wav")}].map(f => f.name)`);
  report.attach = await js(`filmcraft.attachFiles([${file(media.a, "clip.wav")}, ${file(media.b, "stray.wav")}, ${file(media.a, "dir/x.wav")}, ${file(media.a, "dir\\y.wav")}])`);
  const separated = report.names.filter((n) => /[\\/]/.test(n));
  g.attachResult = JSON.stringify(report.attach.paths) === JSON.stringify(["/files/clip.wav", "/files/stray.wav"])
    && report.attach.online.length === 1 && separated.length >= 1
    && report.attach.errors.length === separated.length && separated.every((n, i) => report.attach.errors[i].startsWith(n + ": "));
  report.files = (await js("filmcraft.files()")).map((f) => f.path);
  g.separatorNamesNotRegistered = !report.files.some((p) => /dir/.test(p));
  report.after = await js('filmcraft.execute("media.status")');
  g.onlineAfter = report.after.length === 1 && report.after[0].status === "online";
  g.noBinItem = report.after.length === report.before.length;
  g.noUndoStep = JSON.stringify(historyBefore) === JSON.stringify(await js('filmcraft.execute("history.list")'));
  g.sameDocument = docBefore === await snapshot();

  // the same name again, other bytes: the first file stays
  report.again = await js(`filmcraft.attachFiles([${file(media.b, "clip.wav")}])`);
  g.againListedNoError = JSON.stringify(report.again.paths) === JSON.stringify(["/files/clip.wav"]) && report.again.errors.length === 0;
  g.firstFileKept = (await readHash("/files/clip.wav")) === sha(media.a);
  await sleep(1000);
  g.stillOnline = (await js('filmcraft.execute("media.status")'))[0].status === "online";
  g.noDialogAfterAttach = (await linkMedia()) === 0;
  await shot("2-after-attach");
  report.panics = logs.filter((l) => /panicked|EXCEPTION/.test(l));
  g.noPanics = report.panics.length === 0;
  report.ok = Object.values(g).every(Boolean);
} catch (e) {
  report.error = String(e && e.stack || e);
  report.ok = false;
}
report.logTail = logs.slice(-10);
console.log(JSON.stringify(report, null, 2));
proc.kill();
process.exit(report.ok ? 0 : 1);
