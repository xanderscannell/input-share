# Changelog

Notable changes to input-share. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Bugs are investigated and tracked
in [docs/BUGS.md](docs/BUGS.md); a fix links its entry there.

## [Unreleased]

Nothing yet.

## [0.1.1] - 2026-10-01

### Fixed

- Launching the app while it is already running (for example hidden in the
  tray, still sharing) brings the running copy forward instead of opening a
  second, idle window ([BUG-001](docs/BUGS.md#bug-001-a-client-connects-even-though-the-host-has-not-started-sharing)).
- The address box on the host list no longer loses focus and clears itself
  every second ([BUG-002](docs/BUGS.md#bug-002-the-manual-address-box-loses-focus-and-clears-itself)).
- The host list keeps refreshing after a visit to Key or Settings
  ([BUG-003](docs/BUGS.md#bug-003-the-host-list-sometimes-never-shows-any-computers)).
- Disconnect, Stop sharing and the tray's Stop and Quit no longer freeze the
  window or get stuck forever, which could make Windows end the app as hung
  or leave it needing Task Manager. A stop now finishes within about a
  second even when Windows refuses to shut the connection's socket down or
  the other computer never closes its end
  ([BUG-004](docs/BUGS.md#bug-004-the-app-sometimes-crashes-when-quitting-or-disconnecting)).
- A mistyped address no longer leaves the screen on "Connecting..."; it
  stays on the host list with the error and the typed text
  ([BUG-005](docs/BUGS.md#bug-005-a-mistyped-address-leaves-the-client-stuck-on-connecting)).
- Demo mode: connecting to a host from the list works (its fake host used
  to shut down as the list closed).

### Added

- `tools/check_gui.mjs`: drives the demo GUI and checks the fixes above.
- `--demo-tray quit`: drives the tray's Quit in demo mode.
- README: both computers' networks must be set to Private. On a Public
  network a computer cannot see hosts or accept connections (BUG-003).

## [0.1.0] - 2026-09-29

The first release.

### Added

- Wire protocol with strict encode and decode, carried over a framed Noise
  `NNpsk0` transport with a shared 256-bit key, heartbeat and timeout.
- Server: low-level mouse and keyboard hooks, crossing at a chosen edge,
  cursor parking and hiding while control is remote, panic hotkey
  (Ctrl+Alt+Shift+Esc).
- Client: injection with `SendInput` (scancodes, absolute moves, DPI aware),
  reconnect with backoff, release of held keys and buttons on exit.
- Multi-monitor layouts: crossing and landing on the outermost real monitor
  at each height, refreshed when monitors change.
- LAN discovery: UDP beacons from the sharing computer, a host list with
  expiry and key fingerprints on the other.
- CLI (`input-share`) and a Tauri GUI (`input-share-gui`) with a desk
  diagram, sharing and connecting screens, key and settings pages, a tray
  icon that mirrors the screen arrangement, and a `--demo` mode.
- Settings locked while sharing, looking for hosts or connected.
- App icon, README and MIT license.

[Unreleased]: https://github.com/xanderscannell/input-share/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/xanderscannell/input-share/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/xanderscannell/input-share/releases/tag/v0.1.0
