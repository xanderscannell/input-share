# Bugs

One record per bug, written in the style of an architecture decision record:
what was seen, what the investigation found, and what was decided. A record
is never deleted. When a fix lands, its status changes and the changelog
links back here.

**Status:** `Open` (cause not known), `Diagnosed` (cause known, not fixed),
`Fixed` (with the commit), `Won't fix` (with the reason).

**Evidence levels**, so a record never claims more than was checked:

- *Observed*: seen on a running system (a process list, a socket, a log).
- *Traced*: followed through the code, not yet reproduced in the app.
- *Suspected*: plausible, not checked.

## Template

```markdown
## BUG-NNN: one-line symptom

- **Status:** Open
- **Reported:** YYYY-MM-DD
- **Area:** gui | core | cli

### Symptom
What the user saw, in their words where possible.

### Investigation
What was checked and what it showed, each finding marked Observed, Traced
or Suspected. File and line references.

### Cause
The root cause once known, or "Unknown" with the open leads.

### Decision
What will be done about it and why. Filled in before the fix is written.

### Fix
Commit and test that guards it. Empty until fixed.
```

---

## BUG-001: a client connects even though the host has not started sharing

- **Status:** Diagnosed (likely cause; confirm on the host)
- **Reported:** 2026-10-01
- **Area:** gui

### Symptom

The client can connect to the host without anyone pressing "Share" on the
host.

### Investigation

- *Observed* (this machine, 2026-10-01): two `input-share-gui.exe` processes
  from `target\release`. PID 38444, started 17:58, has no visible window and
  owns the TCP listener on `0.0.0.0:24800`. PID 60340, started 18:03, is the
  visible window. So an earlier instance was still sharing from the tray
  while a second, idle window was open.
- *Traced*: closing the window while sharing hides it to the tray instead of
  quitting (`gui/src/main.rs:226-235`). Nothing stops a second launch: there
  is no single-instance check, so starting the app again opens a fresh idle
  window next to the one still sharing.
- *Traced*: Stop sharing does release the port. `stop_sharing` drops the
  beacon and calls `ServerHandle::stop`, which ends the accept thread and
  with it the `TcpListener` (`gui/src/backend.rs:180-185`,
  `core/src/server.rs:354-372`). No path keeps a listener open after a stop.
- The handshake still requires the shared key, so this is not a way in for
  a computer without the key.

### Cause

A hidden earlier instance that is still sharing, plus no single-instance
guard. Not yet checked on the host machine, if that is a different computer
from this one.

### Decision

Pending.

### Fix

---

## BUG-002: the manual address box loses focus and clears itself

- **Status:** Diagnosed
- **Reported:** 2026-10-01
- **Area:** gui

### Symptom

On "Use another computer's keyboard and mouse", the box for typing the
host's address keeps losing focus and dropping whatever was typed, which
makes it nearly impossible to use.

### Investigation

- *Traced*: while browsing, `pollHosts` runs every second and calls
  `render()` (`gui/ui/app.js:389-398`). `renderView()` empties `#view` and
  rebuilds the screen from the `t-browsing` template
  (`gui/ui/app.js:126-127`, `145-167`). That template contains the manual
  address form (`gui/ui/index.html:82-88`), so once a second the input is
  replaced by a new empty one: focus and text are both lost.
- Matches the report exactly: the loss happens about once a second, whether
  or not any host was found.

### Cause

The once-a-second host list refresh rebuilds the whole browsing screen,
including the address input, instead of updating only the list.

### Decision

Pending.

### Fix

---

## BUG-003: the host list sometimes never shows any computers

- **Status:** Diagnosed (one cause traced; others still possible)
- **Reported:** 2026-10-01
- **Area:** gui, possibly network

### Symptom

Sometimes the list of computers that are sharing does not appear.

### Investigation

- *Traced*: the polling timer stops itself as soon as the screen is not
  "browsing" (`gui/ui/app.js:392`). Opening Key or Settings while looking
  stops it. Coming back through the wordmark sends `go("home")`, which
  returns to "browsing" for an idle client (`gui/ui/app.js:404`), but nothing
  restarts the poll. The list is then frozen at whatever it held when the
  user left, which is empty if no beacon had arrived yet. Only "Stop
  looking" and starting again recovers it.
- *Observed* (this machine): Windows Firewall allows `input-share-gui.exe`
  inbound on Private networks only, and both Ethernet and Tailscale are
  Private here. Not checked on the other computer. Firewall rules are tied to
  the exe path, so a build from a different folder gets a new prompt, and a
  network marked Public would block the beacons.
- *Observed* (this machine): Tailscale has interface metric 5, Ethernet 25,
  plus a WSL virtual adapter. Windows sends a `255.255.255.255` broadcast out
  of one interface only. Here the route for it resolves to Ethernet, so this
  machine's beacons should reach the LAN; not checked on the other computer.
- *Suspected*: if the other computer's broadcast leaves on the wrong adapter
  (VPN, virtual switch), its beacons never reach this LAN.

### Cause

At least one: the poll is never restarted after visiting Key or Settings.
Network causes on the other computer are not ruled out.

### Decision

Pending.

### Fix

---

## BUG-004: the app sometimes crashes when quitting or disconnecting

- **Status:** Diagnosed (structural cause; what stalled the client is unknown)
- **Reported:** 2026-10-01
- **Area:** gui, core

### Symptom

Sometimes the whole app crashes when quitting or disconnecting.

### Investigation

- *Observed*: no Application Error, Application Hang or Windows Error
  Reporting entry for `input-share` in this machine's Application event log
  for the last five days.
- *Observed* (reported from the laptop, 2026-10-01): the crash left an
  Application Hang (event 1002) for the app, not an Application Error
  (1000). The process stopped answering window messages and Windows ended
  it; nothing panicked or faulted.
- *Observed* (reported): the laptop was the client and the hang started on
  Disconnect in the window. While it hung, closing the window and Quit from
  the tray did nothing either. That is expected once the main thread is
  stuck: both arrive as messages to that same thread. So the trigger
  matters less than the fact that any stop can block it.
- *Traced*, ruled out: a deadlock between the client thread's status
  `emit` and the waiting main thread. `tauri-runtime-wry` 2.12.0 only waits
  for the main thread in `eval_script` when its `tracing` feature is on,
  and `cargo tree` shows it is off here, so `emit` just posts and returns.
- *Traced*: Windows only calls a window hung after about 5 s without
  answering messages. The waits found below add up to 2 to 3 s, so either
  they are longer in practice or something else blocks the main thread.
- *Traced*: the hook callbacks never unwrap (a panic there would abort the
  process), and the workspace does not set `panic = "abort"`, so a panic on a
  worker thread should not take the process down.
- *Traced*: every stop runs on the GUI's main thread. Tauri runs non-async
  commands on the main thread, and the tray menu handler runs there too.
  Each stop joins worker threads while holding the backend lock:
  - client stop waits for a connect in flight, up to its 2 s timeout
    (`core/src/client.rs:203-208`, `215-223`);
  - server stop waits for the session to drain, up to the 3 s read timeout
    if the other side does not answer (`core/src/net.rs:16`,
    `core/src/server.rs:354-372`), plus the hook thread.
  During that wait the window cannot repaint or respond, which Windows shows
  as "Not responding" and which can look like a crash.
- *Suspected*: the release build has no console, so a panic message is
  invisible; a real crash would leave no trace on screen.

### Cause

A hang on the main thread during a stop. The blocking joins below are the
leading suspect, but they do not yet explain a wait longer than 5 s.

### Decision

Pending. The structural cause is known: stops run on the main thread and
wait there for worker threads, so any stall in a worker freezes the whole
window. What made the client thread take longer than 5 s on the laptop is
not known yet. A debug build run from a terminal on the laptop would show
how far the client got before it stuck (it prints `release-all` when its
session ends).

### Fix
