# Changelog

Notable changes to input-share. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Bugs are investigated and tracked
in [docs/BUGS.md](docs/BUGS.md); a fix links its entry there.

## [Unreleased]

### Fixed

- The address box on the host list no longer loses focus and clears itself
  every second ([BUG-002](docs/BUGS.md#bug-002-the-manual-address-box-loses-focus-and-clears-itself)).
- The host list keeps refreshing after a visit to Key or Settings
  ([BUG-003](docs/BUGS.md#bug-003-the-host-list-sometimes-never-shows-any-computers)).

### Added

- `tools/check_browsing.mjs`: drives the demo GUI and checks both fixes above.

## [0.1.0] - not yet tagged

The first working version: everything up to commit `de6da48`.

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

### Known issues

- BUG-001 to BUG-005 in [docs/BUGS.md](docs/BUGS.md).
