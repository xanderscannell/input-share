// Drive the demo GUI through WebView2's DevTools port and check fixed bugs:
// a typed address survives the host list refresh (BUG-002), the refresh keeps
// running after a visit to Settings (BUG-003), and the window keeps
// responding while a disconnect waits for the client to stop (BUG-004).
// Usage: cargo build -p input-share-gui && node tools/check_gui.mjs
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const port = "9333";
const exe = fileURLToPath(new URL("../target/debug/input-share-gui.exe", import.meta.url));
const gui = spawn(exe, ["--demo"], {
  env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` },
});
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let targets;
for (let i = 0; i < 60 && !targets; i++) {
  try {
    const t = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    if (t.some((x) => x.type === "page")) targets = t;
  } catch {}
  if (!targets) await sleep(500);
}
const page = targets.find((x) => x.type === "page");
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r));
let id = 0;
const pending = new Map();
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (pending.has(m.id)) pending.get(m.id)(m), pending.delete(m.id);
});
const evaluate = (expr) =>
  new Promise((r) => {
    pending.set(++id, (m) => r(m.result?.result?.value));
    ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression: expr, awaitPromise: true, returnByValue: true } }));
  });

await sleep(1500); // boot
let fails = 0;
const check = (name, ok, detail) => {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}: ${detail}`);
  if (!ok) fails++;
};

// BUG-002: type an address while the list refreshes once a second.
await evaluate(`document.querySelector('[data-act=browse]').click()`);
await sleep(1500);
await evaluate(`(() => { const i = document.getElementById('manual-addr'); i.focus(); i.value = '192.168.0.50'; })()`);
await sleep(3500);
const state = await evaluate(`({ focus: document.activeElement && document.activeElement.id, value: document.getElementById('manual-addr').value })`);
check("BUG-002 typed address survives refreshes", state.focus === "manual-addr" && state.value === "192.168.0.50", JSON.stringify(state));

// BUG-003: leave for Settings long enough for the timer to notice, come back,
// empty the list, and require the refresh to fill it again.
await evaluate(`document.querySelector('[data-go=settings]').click()`);
await sleep(1500);
await evaluate(`document.querySelector('[data-go=home]').click()`);
await sleep(200);
await evaluate(`(() => { ui.hosts = []; render(); })()`);
await sleep(2500);
const hosts = await evaluate(`document.querySelectorAll('.hosts li').length`);
check("BUG-003 list keeps refreshing after visiting Settings", hosts === 2, `${hosts} hosts shown`);

// BUG-004: disconnect while a connect is in flight. 192.0.2.1 is TEST-NET-1,
// never routed, so the client sits in its 2 s connect timeout and the stop
// has to wait for it. A main-thread call (tray_state) must still answer.
await evaluate(`(() => { const i = document.getElementById('manual-addr'); i.value = '192.0.2.1:24800'; i.form.requestSubmit(); })()`);
await sleep(300);
await evaluate(`document.querySelector('[data-act=disconnect]').click()`);
await sleep(100);
const ms = await evaluate(`(async () => {
  const t = performance.now();
  await window.__TAURI__.core.invoke("tray_state", { state: "idle", tooltip: "input-share: not sharing" });
  return Math.round(performance.now() - t);
})()`);
check("BUG-004 window answers during a slow disconnect", ms < 500, `main thread answered in ${ms} ms`);
await sleep(2500);
const after = await evaluate(`ui.screen`);
check("BUG-004 the disconnect still finishes", after === "home", `screen ${after}`);

ws.close();
gui.kill();
process.exit(fails ? 1 : 0);
