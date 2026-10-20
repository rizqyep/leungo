# Decisions

Why things are the way they are, where the code alone doesn't say.

## Windows

- **Capture is WASAPI process loopback, excluding Leungo.** Endpoint
  loopback hears Equalizer APO's output, so the analysis would measure its
  own correction. Measured on Windows 10 22H2 with a -20 dB 1 kHz cut in
  Equalizer APO: endpoint loopback showed the cut, process loopback didn't.
  Cost: needs Windows 10 2004 or later.
- **Equalizer APO is fetched, not bundled.** The dashboard downloads one
  pinned installer from SourceForge and refuses it unless its SHA-256
  matches; the manual download page is the fallback. This matches the
  macOS BlackHole flow and keeps GPL-licensed binaries out of Leungo's
  releases. A silent install wouldn't help: the device selection and the
  restart need the user anyway. The pinned hashes were checked against the
  MD5s SourceForge publishes for 1.4.2.
- **Leungo owns `leungo.txt`, not `config.txt`.** It adds one
  `Include: leungo.txt` line. The installer's stock `config.txt` (a demo
  bass boost with -6 dB preamp) is replaced instead, because it would stack
  on Leungo's curve; it is saved as `config.txt.before-leungo`. A
  `config.txt` that isn't the stock one is never rewritten.
- **Writes retry while busy.** Equalizer APO opens its files to reload them
  whenever anything in the folder changes, and Windows refuses to replace an
  open file (sharing violation). Seen on the first run.
- **The engine restarts when the default output changes.** Equalizer APO is
  per device, so the new output may need the Device Selector, and the
  dashboard shows which device it's playing to.
- **A separate `leungo-app.exe` starts the dashboard.** `leungo.exe` must
  stay a console program so terminal commands print; a GUI-subsystem build
  that reattaches to the parent console returns the prompt before output.
  The launcher is GUI-subsystem and starts `leungo gui --quit-on-close`
  with `CREATE_NO_WINDOW`, whose hidden console children (curl, a restarted
  engine) inherit, so no console window ever flashes.
- **Quitting empties `leungo.txt`.** Equalizer APO keeps applying the last
  file after Leungo exits, which would leave the sound changed with nothing
  running. Uninstalling runs `leungo detach`, which also removes the include
  and restores the installer's `config.txt`.
- **The installer is per-user (Inno Setup, `PrivilegesRequired=lowest`).**
  Leungo needs no admin rights; only Equalizer APO's own installer asks.
  Equalizer APO's Device Selector has no command line, so ticking the device
  stays a manual step in its window.
- **A silent install (`/VERYSILENT`) skips Equalizer APO.** Its installer
  needs a person (permission prompt, device window), so a silent setup would
  wait forever; Leungo offers Equalizer APO on first open instead.
