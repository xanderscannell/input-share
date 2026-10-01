// input-share GUI. One state object, one render function. Anything that came
// from the network (hostnames, addresses, errors) is set with textContent.

const tauri = window.__TAURI__;
const invoke = (cmd, args) => tauri.core.invoke(cmd, args);

const ui = {
  boot: null,
  screen: "home", // home | sharing | browsing | client | keys | settings
  role: null, // null | "server" | "client"
  conn: "idle", // idle | waiting | connecting | connected | retrying
  peer: "", // server: the client's IP; client: the host's name
  addr: "", // server: our listening address; client: the host's address
  pointer: "this", // this | other
  reason: "",
  hosts: [],
  error: "",
  confirmReplace: false,
  importNote: "",
};

// ---- Helpers ----

const $ = (sel, root = document) => root.querySelector(sel);
const slot = (root, name) => root.querySelector(`[data-slot="${name}"]`);

function clone(id) {
  return document.getElementById(id).content.cloneNode(true);
}

/** Fill an element with a grouped fingerprint, one span per group of four. */
function fillFingerprint(el, fp) {
  el.textContent = "";
  if (!fp) {
    el.textContent = "No key yet";
    return;
  }
  for (const group of fp.split(" ")) {
    const s = document.createElement("span");
    s.textContent = group;
    el.append(s);
  }
  el.setAttribute("aria-label", fp.split("").join(" "));
}

const hostOnly = (addr) => addr.replace(/:\d+$/, "");
const sideWord = () => (ui.boot.edge === "left" ? "left" : "right");
const otherSideWord = () => (ui.boot.edge === "left" ? "right" : "left");

// ---- The desk diagram ----

const SCREEN_W = 168;
const X_LEFT = 16;
const X_RIGHT = 216;
const X_SOLO = 116;

function renderDesk() {
  const desk = $("#desk");
  const mode = !ui.role ? "solo" : ui.conn === "connected" ? "linked" : "waiting";

  // The server's edge setting says where the client is. The client sees the
  // server on the opposite side.
  const edge = ui.boot ? ui.boot.edge : "right";
  const otherSide = ui.role === "client" ? (edge === "right" ? "left" : "right") : edge;
  const pointer = mode === "linked" ? ui.pointer : "this";

  desk.dataset.mode = mode;
  desk.dataset.pointer = pointer;
  desk.dataset.otherSide = otherSide;
  // Setup pages have nothing to say about control; give their room to content.
  desk.hidden = ui.screen === "keys" || ui.screen === "settings";

  const xThis = mode === "solo" ? X_SOLO : otherSide === "right" ? X_LEFT : X_RIGHT;
  const xOther = otherSide === "right" ? X_RIGHT : X_LEFT;
  $("#screen-this").setAttribute("transform", `translate(${xThis} 4)`);
  $("#screen-other").setAttribute("transform", `translate(${xOther} 4)`);

  const x = (pointer === "this" ? xThis : xOther) + SCREEN_W / 2 - 6;
  $("#pointer").style.transform = `translate(${x}px, ${44}px)`;

  let other = "";
  if (mode === "waiting") other = ui.role === "client" && ui.peer ? ui.peer : "Other computer";
  if (mode === "linked") other = ui.peer;
  $("#label-other").textContent = other;
}

// ---- Status sentence ----

function statusText() {
  if (ui.role === "server") {
    if (ui.conn === "waiting") {
      return ["Waiting for your other computer.", "On the other computer, choose “Use another computer’s keyboard and mouse”."];
    }
    if (ui.conn === "connected" && ui.pointer === "this") {
      return [`Connected to ${ui.peer}.`, `Move the pointer off the ${sideWord()} edge of this screen to control it.`];
    }
    if (ui.conn === "connected") {
      return [`You're controlling ${ui.peer}.`, `Move back across its ${otherSideWord()} edge to return. If you get stuck, press Ctrl+Alt+Shift+Esc.`];
    }
  }
  if (ui.role === "client") {
    if (ui.conn === "connecting") return [`Connecting to ${ui.peer}…`, "This takes a moment."];
    if (ui.conn === "retrying") {
      return [`Can't reach ${ui.peer}.`, "Check that it is sharing and on the same network. Trying again every few seconds."];
    }
    if (ui.conn === "connected" && ui.pointer === "this") {
      return [`${ui.peer} is controlling this computer.`, "Its keyboard and mouse work here until its pointer crosses back."];
    }
    if (ui.conn === "connected") {
      return [`Connected to ${ui.peer}.`, "Its keyboard and mouse take over here when its pointer crosses to this screen."];
    }
  }
  if (ui.screen === "browsing") return ["Choose the computer to use.", ""];
  if (ui.screen === "keys") {
    return ui.boot.fingerprint
      ? ["Both computers need the same key.", "Compare this key with the one on your other computer. Every group must match."]
      : ["Make a key to get started.", "Make one here, then copy it to your other computer, or import the one it already has."];
  }
  if (ui.screen === "settings") return ["Settings", ""];
  return ["Not sharing.", "Choose what this computer should do."];
}

// ---- Screens ----

function renderView() {
  const view = $("#view");
  // The host list refreshes every second; rebuilding the browsing screen each
  // time would replace the address box someone is typing in. Keep it.
  const keep = ui.screen === "browsing" && view.dataset.screen === "browsing";
  if (!keep) view.textContent = "";
  view.dataset.screen = ui.screen;
  const nav = { keys: "keys", settings: "settings" };
  for (const b of document.querySelectorAll(".bar-nav button")) {
    b.toggleAttribute("aria-current", nav[ui.screen] === b.dataset.go);
    if (nav[ui.screen] === b.dataset.go) b.setAttribute("aria-current", "page");
  }

  if (ui.screen === "home") view.append(clone("t-home"));

  if (ui.screen === "sharing") {
    const t = clone("t-sharing");
    slot(t, "addr").textContent = ui.addr || "Starting…";
    fillFingerprint(slot(t, "fp"), ui.boot.fingerprint);
    slot(t, "hint").textContent =
      ui.conn === "connected" ? "Keep this window open or minimized while you work." : "The other computer must use the same key.";
    view.append(t);
  }

  if (ui.screen === "browsing") {
    const t = keep ? view : clone("t-browsing");
    const list = slot(t, "hosts");
    // Unchanged hosts: leave the buttons alone, so focus and hover stay put.
    const hostsKey = JSON.stringify(ui.hosts);
    if (list.dataset.hosts !== hostsKey) {
      list.textContent = "";
      for (const h of ui.hosts) {
        const li = clone("t-host");
        const btn = $("button", li);
        slot(li, "name").textContent = h.name || h.addr;
        slot(li, "addr").textContent = h.addr;
        const key = slot(li, "key");
        key.classList.toggle("paired", h.paired);
        key.append(icon(h.paired ? "check" : "cross"), document.createTextNode(h.paired ? "Same key" : "Different key"));
        btn.dataset.addr = h.addr;
        btn.dataset.name = h.name || h.addr;
        if (!h.paired) {
          btn.setAttribute("aria-disabled", "true");
          btn.title = "This computer has a different key. Import its key first.";
        }
        list.append(li);
      }
      list.dataset.hosts = hostsKey;
    }
    slot(t, "empty").hidden = ui.hosts.length > 0;
    list.hidden = ui.hosts.length === 0;
    if (!keep) view.append(t);
  }

  if (ui.screen === "client") {
    const t = clone("t-client");
    slot(t, "name").textContent = `${ui.peer} (${ui.addr})`;
    fillFingerprint(slot(t, "fp"), ui.boot.fingerprint);
    slot(t, "hint").textContent = "Keep this window open or minimized while you work.";
    view.append(t);
  }

  if (ui.screen === "keys") {
    const t = clone("t-keys");
    fillFingerprint(slot(t, "fp"), ui.boot.fingerprint);
    slot(t, "fp").classList.toggle("missing", !ui.boot.fingerprint);
    slot(t, "hint").textContent = ui.importNote;
    slot(t, "hint").hidden = !ui.importNote;
    slot(t, "confirm").hidden = !ui.confirmReplace;
    slot(t, "key-actions").hidden = ui.confirmReplace;
    view.append(t);
  }

  if (ui.screen === "settings") {
    const t = clone("t-settings");
    for (const r of t.querySelectorAll('input[name="edge"]')) r.checked = r.value === ui.boot.edge;
    $("#port", t).value = ui.boot.port;
    // A running session keeps the settings it started with: lock until stopped.
    if (ui.role) {
      for (const el of t.querySelectorAll("input, button")) el.disabled = true;
      const locked = slot(t, "locked");
      locked.hidden = false;
      locked.textContent =
        ui.role === "server"
          ? "Stop sharing to change these settings."
          : ui.conn === "idle"
            ? "Stop looking to change these settings."
            : "Disconnect to change these settings.";
    }
    view.append(t);
  }
}

function icon(kind) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("aria-hidden", "true");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", kind === "check" ? "M3 8.5 6.5 12 13 4.5" : "M4.5 4.5 11.5 11.5M11.5 4.5 4.5 11.5");
  svg.append(p);
  return svg;
}

function render() {
  if (!ui.boot) return;
  const [status, detail] = statusText();
  $("#status").textContent = status;
  $("#detail").textContent = detail;
  $("#detail").hidden = !detail;
  $("#alert").hidden = !ui.error;
  $("#alert-text").textContent = ui.error;
  renderDesk();
  renderView();
  syncTray(status);
}

// The tray shows the same picture as the desk diagram: which screen is lit,
// on the same side the diagram draws it (renderDesk has just set otherSide).
let trayShown = "";
function syncTray(status) {
  const running = ui.role && ui.conn !== "idle";
  const thisSide = $("#desk").dataset.otherSide === "right" ? "left" : "right";
  const otherSide = thisSide === "left" ? "right" : "left";
  const away = ui.conn === "connected" && ui.pointer === "other";
  const state = !running ? "idle" : away ? `away-${otherSide}` : `here-${thisSide}`;
  const tooltip = running ? `input-share: ${status}` : "input-share: not sharing";
  if (state + tooltip === trayShown) return;
  trayShown = state + tooltip;
  invoke("tray_state", { state, tooltip }).catch(() => {});
}

// ---- Status events from the backend ----

function onStatus({ kind, detail }) {
  switch (kind) {
    case "listening":
      // Also taken when sharing started from outside this window (tray, demo).
      if (!ui.role) Object.assign(ui, { role: "server", screen: "sharing" });
      ui.conn = "waiting";
      // start_sharing returns the LAN address (192.168.x.y); the event only
      // knows the bind address (0.0.0.0), so never let it overwrite.
      if (!ui.addr) ui.addr = detail;
      break;
    case "connecting":
      if (!ui.role) Object.assign(ui, { role: "client", screen: "client" });
      ui.conn = "connecting";
      break;
    case "retrying":
      ui.conn = "retrying";
      ui.reason = detail;
      break;
    case "connected":
      ui.conn = "connected";
      ui.pointer = ui.role === "server" ? "this" : "other";
      if (ui.role === "server") ui.peer = hostOnly(detail);
      if (ui.role === "client") ui.screen = "client";
      ui.error = "";
      break;
    case "remote":
      ui.pointer = ui.role === "server" ? "other" : "this";
      break;
    case "local":
      ui.pointer = ui.role === "server" ? "this" : "other";
      break;
    case "disconnected":
      if (ui.role === "server") ui.conn = "waiting";
      if (ui.role === "client") ui.conn = "connecting";
      ui.pointer = ui.role === "server" ? "this" : "other";
      break;
    case "stopped":
      ui.conn = "idle";
      break;
  }
  render();
}

// ---- Actions ----

async function act(name, el, event) {
  ui.error = "";
  try {
    switch (name) {
      case "share":
        if (!ui.boot.fingerprint) return go("keys");
        ui.role = "server";
        ui.screen = "sharing";
        ui.conn = "waiting";
        render();
        ui.addr = await invoke("start_sharing");
        break;
      case "stop-sharing":
        await invoke("stop_sharing");
        Object.assign(ui, { role: null, conn: "idle", screen: "home", pointer: "this", peer: "", addr: "" });
        break;
      case "browse":
        if (!ui.boot.fingerprint) return go("keys");
        ui.role = "client";
        ui.screen = "browsing";
        render();
        await invoke("start_browsing");
        pollHosts();
        break;
      case "connect":
        if (el.getAttribute("aria-disabled") === "true") {
          ui.error = `${el.dataset.name} uses a different key. Import its key on the Key page first.`;
          break;
        }
        await connectTo(el.dataset.addr, el.dataset.name);
        break;
      case "connect-manual": {
        event.preventDefault();
        const addr = el.elements.addr.value.trim();
        await connectTo(addr.includes(":") ? addr : `${addr}:${ui.boot.port}`, addr);
        break;
      }
      case "disconnect":
        // Back home, like Stop sharing. (Returning to the host list left the
        // client role on with no way out, and discovery was already stopped.)
        await invoke("disconnect");
        Object.assign(ui, { role: null, conn: "idle", screen: "home", pointer: "this", peer: "", addr: "" });
        break;
      case "stop-browsing":
        clearInterval(hostTimer);
        await invoke("stop_browsing");
        Object.assign(ui, { role: null, conn: "idle", screen: "home", pointer: "this", hosts: [] });
        break;
      case "key-new":
        if (ui.boot.fingerprint) ui.confirmReplace = true;
        else ui.boot.fingerprint = await invoke("key_generate", { replace: false });
        break;
      case "key-replace":
        ui.boot.fingerprint = await invoke("key_generate", { replace: true });
        ui.confirmReplace = false;
        ui.importNote = "New key made. Copy it to your other computer.";
        break;
      case "key-keep":
        ui.confirmReplace = false;
        break;
      case "key-reveal":
        ui.importNote = `The key file is ${await invoke("key_reveal")}. Copy it to the other computer, or paste its contents there.`;
        break;
      case "key-import": {
        event.preventDefault();
        const text = $("#key-text").value;
        ui.boot.fingerprint = await invoke("key_import", { text, replace: true });
        ui.importNote = "Key imported. Check that it matches the other computer.";
        break;
      }
      case "save-settings": {
        event.preventDefault();
        const edge = el.elements.edge.value;
        const port = Number(el.elements.port.value);
        await invoke("save_settings", { edge, port });
        Object.assign(ui.boot, { edge, port });
        ui.importNote = "";
        go("home");
        return;
      }
    }
  } catch (e) {
    ui.error = String(e);
  }
  render();
}

async function connectTo(addr, name) {
  await invoke("stop_browsing");
  Object.assign(ui, { screen: "client", conn: "connecting", peer: name, addr });
  render();
  await invoke("connect", { addr });
}

let hostTimer = null;
function pollHosts() {
  clearInterval(hostTimer);
  const tick = async () => {
    if (ui.screen !== "browsing") return clearInterval(hostTimer);
    ui.hosts = await invoke("hosts");
    render();
  };
  tick();
  hostTimer = setInterval(tick, 1000);
}

function go(screen) {
  ui.confirmReplace = false;
  ui.error = "";
  if (screen === "home" && ui.role === "server") screen = "sharing";
  if (screen === "home" && ui.role === "client") screen = ui.conn === "idle" ? "browsing" : "client";
  ui.screen = screen;
  render();
  // The poll stops itself while another screen is open; start it again.
  if (screen === "browsing") pollHosts();
}

document.addEventListener("click", (e) => {
  const nav = e.target.closest("[data-go]");
  if (nav) return go(nav.dataset.go);
  const btn = e.target.closest("button[data-act]");
  if (btn) act(btn.dataset.act, btn, e);
});

document.addEventListener("submit", (e) => {
  const form = e.target.closest("form[data-act]");
  if (form) act(form.dataset.act, form, e);
});

document.addEventListener("change", async (e) => {
  if (e.target.dataset.act !== "key-file") return;
  const file = e.target.files[0];
  if (file) $("#key-text").value = (await file.text()).trim();
});

// ---- Canned states for screenshots (--demo --demo-state NAME) ----

const CANNED = {
  idle: {},
  "sharing-waiting": { role: "server", screen: "sharing", conn: "waiting", addr: "192.168.1.10:24800" },
  "sharing-here": { role: "server", screen: "sharing", conn: "connected", addr: "192.168.1.10:24800", peer: "192.168.1.23", pointer: "this" },
  "sharing-remote": { role: "server", screen: "sharing", conn: "connected", addr: "192.168.1.10:24800", peer: "192.168.1.23", pointer: "other" },
  browsing: {
    role: "client",
    screen: "browsing",
    hosts: [
      { addr: "192.168.1.10:24800", name: "DESKTOP-01", fingerprint: "", paired: true },
      { addr: "192.168.1.57:24800", name: "OFFICE-PC", fingerprint: "", paired: false },
    ],
  },
  "client-connected": { role: "client", screen: "client", conn: "connected", peer: "DESKTOP-01", addr: "192.168.1.10:24800", pointer: "other" },
  "client-remote": { role: "client", screen: "client", conn: "connected", peer: "DESKTOP-01", addr: "192.168.1.10:24800", pointer: "this" },
  error: { role: "client", screen: "client", conn: "retrying", peer: "OFFICE-PC", addr: "192.168.1.57:24800" },
  keys: { screen: "keys" },
  "keys-confirm": { screen: "keys", confirmReplace: true },
  settings: { screen: "settings" },
  "settings-locked": { role: "server", screen: "settings", conn: "waiting", addr: "192.168.1.10:24800" },
};

// ---- Start ----

(async () => {
  ui.boot = await invoke("boot");
  if (ui.boot.theme) document.documentElement.dataset.theme = ui.boot.theme;
  const canned = ui.boot.demo && ui.boot.demo_state && CANNED[ui.boot.demo_state];
  if (canned) {
    Object.assign(ui, canned);
  } else {
    await tauri.event.listen("status", (e) => onStatus(e.payload));
    // Stop from the tray menu: everything has stopped; go back home.
    await tauri.event.listen("tray-stop", () => {
      clearInterval(hostTimer);
      Object.assign(ui, { role: null, conn: "idle", screen: "home", pointer: "this", peer: "", addr: "", error: "" });
      render();
    });
  }
  // Skip the pointer's glide on first paint.
  const pointer = $("#pointer");
  pointer.style.transition = "none";
  render();
  requestAnimationFrame(() => requestAnimationFrame(() => (pointer.style.transition = "")));
})();
