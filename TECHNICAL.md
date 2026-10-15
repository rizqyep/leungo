# Technical notes

How Leungo works inside. For using it, see the [README](README.md). For
changing it, see [CONTRIBUTING.md](CONTRIBUTING.md).

Numbers below are the shipped defaults (`crates/leungo-core/src/config.rs`).
Everything marked *unverified* has not been checked against real listening or
real hardware.

## Goal and approach

Leungo measures the music that is playing and applies a small, bounded
parametric EQ correction, per track. It does not try to make every song match
one target curve. It separates three things:

| Layer | What it is | Source of truth |
|---|---|---|
| Output device | Correction for the headphones or IEMs themselves | A measurement-based profile (`eq.device_curve`), fixed |
| Track | Bumps and dips in this song's balance that are outside normal | Measured live, per track |
| Taste | Bass, warmth, presence, air offsets | The user (`sound` controls) |

The track layer deliberately leaves the song's own spectral tilt alone and
corrects only the residual outside a dead zone. Treating "flat" as the target
makes every real recording look wrong, because music has an energy-per-band
slope.

## Data flow

```
audio source (platform)            SampleSource
  -> frames (FFT 2048, hop 1024, 48 kHz)
  -> per-frame 1/3-octave levels, silence gate
  -> track boundary: player track id (preferred), otherwise silence gap
  -> robust shape + reliability + transient fraction   (leungo-analysis)
  -> archetype label (once per track)
  -> remove tilt, dead zone, cap, scale by strength    (leungo-eq)
  -> ridge least squares onto 8 peaking bands
  -> add device curve and sound signature, shift for headroom
  -> EqApplier (platform)
```

Platform-specific code is limited to two traits in `leungo-core`:

- `SampleSource::next() -> SourceEvent` yields audio chunks (or an
  end-of-stream/device event).
- `EqApplier::apply_eq(&[f64])` and `set_bypass(bool)` push the 8 gains out.

The engine knows nothing about PulseAudio, EasyEffects, CoreAudio or BlackHole.

## Crates

| Crate | Responsibility |
|---|---|
| `leungo-dsp` | RBJ biquads, magnitude response, direct-form-I state, shelf math |
| `leungo-eq` | Archetypes, target shaping, device-profile fitting, signature, the least-squares solver |
| `leungo-analysis` | FFT spectrum, features, robust shape tracker, detector, classifier (rule-based and fitted) |
| `leungo-core` | Config and presets, engine state machine, traits, paths, offline analysis |
| `leungo-capture-pipewire` | Linux capture through the PulseAudio API |
| `leungo-applier-ee` | Writes and loads the EasyEffects preset, bypass |
| `leungo-players` | Track ids and now-playing: MPRIS (Linux), AppleScript (macOS) |
| `leungo-standalone` | macOS engine: loopback capture, resampling, drift control, in-process filters |
| `leungo-cli` | The `leungo` binary, dashboard server, setup flows |

`leungo-capture-pipewire` is empty off Linux and `leungo-standalone`'s audio
I/O is behind the `audio-io` feature, so the workspace builds everywhere and the
pure logic (resampler, drift controller, filter chain) is tested on Linux.

## The engine

`leungo-core/src/engine.rs`. One thread, one loop over chunks from the source.

States: `Idle` (silence or no track), `Analyzing` (collecting the first
window), `Active` (a curve is applied, refining).

1. **New track.** The player reports a new track id (or, with no player, a
   silence gap of `gap_s = 1.2` s followed by sound). The engine resets the
   spectrum memory and enters `Analyzing`. The id from the player is
   authoritative: pause and resume of the same id does not re-analyze, and a
   silence gap alone does not count as a new track when ids are available.
2. **First window.** After `window_s = 6` s of non-silent audio (or
   `max_wall_s = 20` s wall time) the engine measures, classifies, solves and
   applies the curve immediately. Nothing from the previous track is blended in.
3. **Refinement.** While `Active`, every `refine_interval_s = 5` s it
   re-solves from the running measurement and moves each band by at most
   `refine_max_step_db = 1.5` dB, and at most `refine_alpha = 35%` of the
   remaining gap. It acts only if some band still differs by at least
   `minimum_change_db = 1.0` dB.
4. **Deferred switching.** A refinement change is queued and applied at a
   quiet moment: a short dip in level of about `defer_dip_db = 4` dB below the
   recent average, or after `defer_max_s = 10` s regardless. New-track curves
   are never deferred.
5. **Archetype lock.** The archetype and its confidence are decided once from
   the first window and reused by refinement, so the target tilt does not flip
   between sections.

There is no section or "drastic change" detector. Section changes are followed
only through the slowly moving measurement.

The tuning presets (Conservative, Balanced, Aggressive) are a bundle of eight
of these values (strength, dead zone, cap, refine interval, refine fraction,
step, defer time, shape window). The preset is polled from a file (`tuning`) and applied
live; `Custom` uses the values in the config file.

## Measurement

`leungo-analysis`, mainly `robust.rs` and `spectrum.rs`.

- **Level normalization.** Each frame's 1/3-octave levels are normalized by
  that frame's own overall level, so playback volume does not move the result.
- **Gating.** Frames more than 10 dB below the *median* frame level are
  ignored. The median, not mean power, is used so one loud burst cannot gate
  out the quiet majority. (An earlier mean-power gate, as in BS.1770, did that.)
- **Shape.** The per-band median over the last `shape_window_s = 20` s.
- **Reliability** (0..1). The first and second halves of the shape window are
  measured separately and compared: 1.0 means they agree, 0 means they differ by
  6 dB rms or more. It scales correction strength:
  `strength * (0.5 + 0.5 * reliability)`. It says how repeatable the
  measurement is, not whether the result sounds right.
- **Transient fraction.** The share of band energy that is a short excess over
  each band's own 9-frame temporal median. It is a proxy for percussive
  material, which is expected to have more bass and treble, so those
  corrections are reduced by `percussive_relief = 0.5`. The mapping from
  fraction to 0..1 (0.15 to 0.45) is a guess, *unverified* on real music; the
  values are logged so it can be fitted.

Limits: the 2048-point FFT at 48 kHz has ~23 Hz bins, so the effective floor is
about 50 Hz. 1/3-octave bands narrower than a bin are dropped by the solver.

## Solver and target shaping

`leungo-eq`.

1. Take the measured 1/3-octave shape.
2. Fit a robust line to the shape in log-frequency (Theil-Sen: the median of
   all pairwise slopes, with a median intercept) and subtract it. A plain
   least-squares line is dragged by a wide bass bump and cancels its own
   correction; the median-slope fit does not.
3. Shrink the residual by `deadzone_db = 2` (soft threshold) and clamp it to
   `±residual_cap_db = 6`. Points outside 50 Hz to 10 kHz get no correction
   (sub-bass and codec cutoffs). The dead zone can be set per point, which is
   how percussive material is given more room.
4. Add the archetype's small tilt (at most 1.5 dB per band) and the user's
   `base_curve`.
5. Scale by `strength = 0.5`, reliability, and classifier confidence, and
   reduce bass/treble by the percussive relief.
6. Fit **8 peaking biquads** at 63, 125, 250, 500, 1k, 2k, 4k, 8k Hz, Q 1.41,
   each limited to ±12 dB, by ridge-regularized least squares (`ridge = 0.05`)
   on the log-frequency axis.
7. Add the device curve (scaled by `device_strength = 0.6`) and the sound
   signature (shelves and a presence/air shape), then shift the whole curve so
   the largest boost is 0 dB. The result never raises the level, so there is no
   clipping risk from the EQ.

Eight broad bands cannot follow narrow notches (for example a 5 kHz IEM
resonance). The device fit reports its rms error for that reason.

## Device profiles

`leungo device <files...>` reads one or more AutoEq `ParametricEQ.txt` files
(`Preamp`, then `Filter N: ON PK|LSC|HSC Fc … Gain … Q …`), evaluates each
filter set on the 1/3-octave grid, averages the responses in dB across files
(`consensus_db`), and fits the 8 bands to that mean. The result is written to
`eq.device_curve` in the config.

Using several independent measurements is deliberate: a single unit or coupler
can be an outlier. Which target curve AutoEq corrected
toward is *unverified*. The profile data in `profiles/` is not published in the
repo, because the licensing of the underlying measurement data was not checked.

## Classifier

`leungo-analysis/src/classifier.rs`, `fitted.rs`.

Five archetypes: bass-heavy, vocal-centric, guitar-band, bright-acoustic,
neutral. The default classifier is rule-based: nearest prototype in a
four-number descriptor space (bass relative to mids, treble relative to mids,
crest factor, spectral centroid; see `descriptors`). Confidence is
the normalized margin to the runner-up prototype, so it is low by construction
(typically 10 to 25%) and is a "how clear is the boundary" number, not a
probability of being right.

The prototypes were set by hand and were not calibrated against music. A
`leungo fit` command fits them to the tracks logged in `tracks.jsonl`
(needs about 30 distinct tracks that form more than one group) and replaces
the margin with a probability. The archetype only changes the target by at most
1.5 dB before strength scaling, so its audible effect is small.

## Platform backends

### Linux

- **Capture.** The PulseAudio compatibility API (works on PipeWire via
  `pipewire-pulse`). When EasyEffects is running, the source is
  `easyeffects_sink.monitor`, the signal *before* the EQ chain, so the analysis
  never measures its own correction. Otherwise `@DEFAULT_MONITOR@`.
- **Apply.** The daemon writes the preset
  `~/.local/share/easyeffects/output/leungo.json` and asks EasyEffects to load
  it. The file carries both the EasyEffects 7 (`equalizer`) and 8
  (`equalizer#0`) keys.
- **Tracks.** MPRIS over D-Bus: `PropertiesChanged` on
  `org.mpris.MediaPlayer2.Player`, seeded at startup with each player's current
  track. `Metadata` values arrive wrapped in an extra variant layer, which an
  earlier version of the monitor mishandled.
- **Setup.** If the `easyeffects` command is not on `PATH` the dashboard shows
  the distro-specific install command and waits.

### macOS

macOS has no system audio tap and no EasyEffects, so `leungo-standalone` does
the whole job: capture the BlackHole loopback input, resample, filter with 8
in-process biquads (smoothed gain changes), and play to the real output.

- **Clocks.** The two devices run on separate clocks. A small controller
  absorbs the drift. It is unit-tested against simulated drift and *unverified*
  on real hardware over long sessions.
- **Routing.** The app changes the system output to BlackHole while running and
  restores it on exit. A wrong or missing restore leaves the system pointed at
  BlackHole.
- **Tracks.** Spotify and Music are polled with AppleScript. Automation
  permission is requested on first use.
- **Latency.** About `latency_ms = 60` ms is added.

## State, config and files

| What | Where |
|---|---|
| Config | `~/.config/leungo/config.toml` |
| Saved choices next to the config | `classifier.json` (fitted classifier), `devices.json` (chosen devices), `ui.json` (simple or advanced), `route.json` (macOS output routing) |
| Runtime state | `~/.cache/leungo/`: `status.json`, `bypass`, `tuning`, `signature.json` |
| Track log (fitting input) | `~/.local/share/leungo/tracks.jsonl` |
| EasyEffects preset | `~/.local/share/easyeffects/output/leungo.json` |

macOS uses the platform equivalents (`~/Library/Application Support/leungo`).
Directories left over from the earlier name `adaptive-eq` are migrated on first
run and never overwrite an existing `leungo` directory.

The config rejects unknown keys on purpose, so a typo fails loudly.

## Dashboard server

`leungo gui` runs the engine and a small HTTP server on `127.0.0.1:8431`. It
serves one self-contained page (`gui.html`, no external resources). It binds to
loopback only. There is no authentication, because it is not reachable from
other machines; any local process can use it.

| Route | Purpose |
|---|---|
| `GET /` | the page |
| `GET /api/state` | status, spectrum, now playing, gains, setup state |
| `GET /api/devices`, `POST /api/devices` | list and choose capture/output (macOS), or the fixed path (Linux) |
| `POST /api/bypass/on` and `/api/bypass/off` | adaptive layer on or off |
| `POST /api/tuning/<preset>` | tuning preset |
| `POST /api/sound` | sound signature and device strength |
| `POST /api/mode/simple` and `/api/mode/advanced` | UI mode |
| `POST /api/setup/install` | macOS: download and open the BlackHole installer |
| `POST /api/bye` | the window closed (used for quit-on-close) |

Window opening (`gui.open`): `app` launches a Chromium-family browser with
`--app=URL`, which is a chromeless window but still a browser process;
`browser` and `none` are the alternatives. There is no native toolkit window.

## Testing

- Unit tests live next to the code. The engine has integration tests
  (`crates/leungo-core/tests/engine.rs`) that drive it with synthetic audio and
  a recording applier. They cover: every new track is analyzed and applied,
  a pause and resume of the same track keeps its preset, refinement waits for a
  quiet moment, the label is decided once per track, the same track gets the
  same curve whatever came before it, and playback level does not change the
  curve.
- Synthetic signals only. There is no regression corpus of real music, because
  none is available to ship. The values most dependent on real music
  (percussive mapping, dead zone, strength) are therefore *unverified by test*.
- CI builds, lints and tests on Linux and on an Apple silicon macOS runner. The
  cpal audio code is compiled there but not executed against a device.

## Known gaps

- No per-output-device profiles: one `device_curve` is applied to whatever is
  playing.
- No awareness of playback level (equal-loudness effects), because the monitor
  signal is taken before the volume control.
- The classifier is weak and the genre labels are coarse; a style prior from
  metadata was investigated (MusicBrainz) but not built.
- 8 bands and ±12 dB: not a room correction or a precise headphone EQ.
