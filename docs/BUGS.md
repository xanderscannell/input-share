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

- **Status:** Fixed
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

Allow one copy of the GUI per signed-in user. A second launch asks the
running copy to show its window (also when it is hidden in the tray) and
exits. That is what launching the app again almost always means, and it
makes a forgotten copy impossible to miss.

- Done with a named event (`Local\input-share-gui`) through the `windows`
  crate core already uses (one more feature, `Win32_Security`), instead of
  adding `tauri-plugin-single-instance`. The first copy waits on the event
  and shows its window when it fires. The second copy signals it and first
  calls `AllowSetForegroundWindow`, since Windows does not let a background
  process take the foreground on its own.
- Demo mode is exempt, so the demo checks can run next to the real app.
- If creating the event fails, the app starts anyway: a missing guard is
  better than an app that will not open.

### Fix

`core/src/win.rs` (`single_instance`), used at the top of `main` in
`gui/src/main.rs`. Guarded by the unit test
`a_second_copy_wakes_the_first_and_is_told_to_exit`. *Observed* with two
real-mode debug copies: the second exited by itself (code 0) and the first,
minimized, came back to the front. The tray case uses the same
`show_window` as the tray's "Show input-share", already checked with
`--demo-tray close-show`. A window hidden from outside the app (not through
Tauri) does not come back, because Tauri still thinks it is shown; the app
never hides itself that way.
*Observed* (2026-10-01): confirmed on both computers in the release build.

---

## BUG-002: the manual address box loses focus and clears itself

- **Status:** Fixed
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
- *Observed*: reproduced in the demo GUI by `tools/check_gui.mjs`. After
  typing into the box and waiting 3.5 s, the box was empty and had lost
  focus, on three runs out of three.

### Cause

The once-a-second host list refresh rebuilds the whole browsing screen,
including the address input, instead of updating only the list.

### Decision

Rebuild the browsing screen only when arriving on it. While it stays open,
update the host list in place, and rebuild the host buttons only when the
list actually changed, so a focused or hovered button is not replaced
either. A smaller change than splitting the list into its own render
function, and it covers every caller of `render()` on that screen, not only
the poll.

### Fix

`gui/ui/app.js`: `renderView()` keeps the browsing screen when it is already
showing. Guarded by `tools/check_gui.mjs` (run it after
`cargo build -p input-share-gui`): it failed on the old code and passed on
the fix in every run. *Observed* (2026-10-01): confirmed working in the
release build on the real computers.

---

## BUG-003: the host list sometimes never shows any computers

- **Status:** Fixed (GUI cause); network cause found (Public network profile), documented in the README
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
- *Observed*: reproduced in the demo GUI by `tools/check_gui.mjs`. After
  1.5 s on Settings and back, an emptied list stayed empty, on three runs
  out of three.
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
- *Observed* (reported, 2026-10-01, a third laptop): as client it never
  listed any host; as host the other computer kept retrying and could not
  connect; as client with a typed address it connected and worked. Only
  traffic arriving at that laptop failed. Its network profile was
  **Public**.
- *Traced*: the README tells people to allow the firewall prompt on Private
  networks only, so on a Public network Windows blocks the incoming beacons
  and connections, which matches every symptom. Not yet checked: that
  laptop's firewall rule itself, and that switching to Private fixes it.

### Cause

Two separate causes. In the app: the poll was never restarted after
visiting Key or Settings. On the network: a computer whose network is
marked Public blocks everything coming in, given the app's Private-only
firewall rule.

### Decision

Restart the poll whenever `go()` lands on the browsing screen. The poll
already stops itself when the screen changes, so starting it on the way back
is the one missing half.

For the Public network: no code change. Allowing the app on Public networks
as well would accept connections on cafe and airport networks too; the
right fix is marking the home network Private. The README now says so in the
setup steps and under Network and security, with the symptoms and how to
check. The VPN adapter lead stays a suspicion: it has not been seen.

### Fix

`gui/ui/app.js`: `go()` calls `pollHosts()` when it lands on browsing.
Guarded by the same `tools/check_gui.mjs`. *Observed* (2026-10-01):
confirmed working in the release build on the real computers.

---

## BUG-004: the app sometimes crashes when quitting or disconnecting

- **Status:** Fixed (two parts: the frozen window, then the stuck stop)
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

- *Observed* (reported, after the first fix): looping connect and
  disconnect on the laptop got stuck again. The window kept answering, but
  Disconnect, the tray's Stop and Quit all did nothing, and the app had to
  be ended in Task Manager. All three wait for the same backend lock, so one
  stop never finished: stuck, not just slow.
- *Observed* (reproduced here): the demo GUI as client and the CLI server
  (script mode, on loopback), connect and disconnect in a loop with random
  waits. Stuck on round 139 of one run and round 265 of another. With
  temporary debug output, the stuck round showed:
  `stop: shutdown Err(Os { code: 10057, kind: NotConnected, ... })`.
  The client's stop ends the session by shutting down a `try_clone` copy of
  the socket, and Windows sometimes refuses that with `WSAENOTCONN` while
  the connection is live. The error was ignored. The session loop never
  checked the stop flag, and the server's heartbeat every second kept its
  read from timing out, so the stop waited forever while holding the lock.
  The server's log still showed the session as connected, which confirms
  that no FIN was ever sent.
- *Traced*: the server's stop had the same weakness. Its session only ended
  when the client closed. *Observed*: a new test with a client that keeps
  sending heartbeats and never closes hung the server's stop (test failed
  after 3 s).

### Cause

Two layers. The window froze because every stop ran on the main thread.
The stop itself could hang forever because ending a session depended on
the socket shutdown succeeding (client) or the other side closing (server),
and Windows can refuse the shutdown of a duplicated socket on a live
connection.

### Decision

Take every wait off the main thread rather than chase what made one stop
slow. Whatever a worker thread waits on, the window must keep answering;
the slow stop itself is harmless once it no longer freezes the UI.

- Every command that takes the backend lock runs on Tauri's blocking pool
  through one helper (`on_backend`). Not `#[tauri::command(async)]`: that
  runs the body on a Tokio worker, and a join of several seconds would tie
  one up. The lock still serializes commands, so a Connect right after a
  Disconnect waits for the old client to finish, as before.
- The main thread never waits for the lock. The close button uses
  `try_lock` and treats a busy backend as running (hide to the tray). The
  tray's Stop and Quit run on their own thread; Quit exits only after
  everything is released, so a hidden cursor is always restored.
- Left as is: the shutdown in `RunEvent::Exit` (after Quit it has nothing
  left to do; on logoff it is the last chance to release everything).
- Not done: a "Disconnecting..." state in the window. A slow stop now leaves
  the button looking idle until it finishes. Add it if that is confusing.

Second part: stop no longer depends on the socket shutdown. Both session
loops check the stop flag after every message they read; heartbeats arrive
every second, so a stop ends within about a second however the socket
behaves. The shutdown stays as the fast path. The heartbeat thread also ends
once its sender is shut down, even if the OS call fails, because its copy of
the socket would otherwise keep the connection open by itself.

### Fix

`gui/src/main.rs`. Guarded by `tools/check_gui.mjs`: it disconnects while a
connect to an unrouted address is in flight and times a call that needs the
main thread. Old code: 1749 ms, blocked behind the stop. Fixed: 3 ms, on
three runs out of three, and the disconnect still completes. The tray's Quit
and Stop and the close button were checked with `--demo-tray` (new `quit`
sequence): Quit and close while idle exit with code 0, Stop and
close-then-show keep running and Stop returns to the start screen.

Second part: `core/src/client.rs`, `core/src/server.rs`, `core/src/net.rs`.
Guarded by `session_ends_on_stop_without_a_socket_shutdown` (client unit
test: fails without the new check, passes with it) and
`stopping_the_server_is_prompt_even_if_the_client_never_closes`
(`core/tests/lifecycle.rs`: failed before the fix, passes after). The
reproduction loop then ran 500 rounds with no stuck stop. Windows refused the
socket shutdown twice in that run, and both stops still finished, in 561 ms
and 873 ms.
*Observed* (2026-10-01): confirmed on the laptop as client, repeated
connect and disconnect with no more hangs.

---

## BUG-005: a mistyped address leaves the client stuck on "Connecting"

- **Status:** Fixed
- **Reported:** 2026-10-01 (found while fixing BUG-002)
- **Area:** gui

### Symptom

Typing something that is not an IP address (a hostname, a typo) and
pressing Connect shows the error, but the screen also changes to
"Connecting to ...". Nothing is connecting, and the host search has
already stopped.

### Investigation

- *Traced*: `connectTo` stops browsing and switches to the client screen
  with `conn: "connecting"` before calling `connect`
  (`gui/ui/app.js`, `connectTo`). The backend rejects anything that does not
  parse as `IP:port` (`gui/src/backend.rs:251`), and nothing puts the screen
  back when that call fails.
- *Observed*: in the demo GUI, submitting `not-an-ip` left `screen:
  "client"`, `conn: "connecting"`, the status "Connecting to not-an-ip…",
  and the alert "not-an-ip:24800 is not an address like
  192.168.1.20:24800."
- Disconnect still gets out of it, back to the start screen.

### Cause

The client screen is shown optimistically and never undone when `connect`
fails.

### Decision

Ask the backend to connect before leaving the host list, and switch to the
client screen only once it has accepted the address. `connect` returns at
once (the connecting happens on the client's own thread), so a valid address
looks the same as before. A refused one changes nothing: still on the host
list, the error shown, the typed text kept (BUG-002), the list refreshing.

Two things this exposed:

- Status events can now arrive before the screen switch. Setting "connecting"
  unconditionally would overwrite an early "connected" and leave the screen
  on "Connecting..." for good, so it is set only if nothing has moved it on.
  *Observed*: without that guard the new check fails on every run.
- *Observed*, pre-existing: in demo mode, leaving the host list also shut
  down the fake host, so connecting to it from the list never worked (the old
  code fails the new check the same way). Real mode was never affected. The
  demo's fake hosts now stay up while a client uses them and go on
  disconnect.

### Fix

`gui/ui/app.js` (`connectTo`) and, for the demo, `gui/src/backend.rs`
(`stop_browsing`, `disconnect`). Guarded by `tools/check_gui.mjs`: a refused
address stays on the host list (failed before, passes now), and picking a
listed host ends on "Connected" (passes, three runs of three). The backend
test `demo_browsing_finds_two_hosts_and_connects_to_the_paired_one` now
leaves the list after connecting, as the GUI does.
