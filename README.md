# leungo

An adaptive per-track parametric EQ. It listens to whatever your system is
playing, estimates the track's spectral balance and musical character at the
start of each song, and retunes an 8-band parametric EQ so you stop
hand-adjusting settings for every track.

The engine is one Rust codebase. EQ delivery is per platform, because every OS
injects system-wide audio differently:

| Platform | EQ host | Status |
|---|---|---|
| Linux (PipeWire or PulseAudio) | EasyEffects | Working (MVP) |
| Windows | Equalizer APO config generation | Planned |
| macOS | In-process DSP via BlackHole (planned) | Not working yet |

## How it works

```
capture (default-sink monitor)
  -> FFT spectrum, long-term average per track
  -> silence gate + track-change detection (player track ids, silence gaps)
  -> feature extraction (band energies, centroid, rolloff, crest)
  -> archetype classification (bass-heavy / vocal-centric / guitar-band /
     bright-acoustic / neutral)
  -> target curve (base curve + archetype delta)
  -> least-squares fit of 8 peaking biquads
  -> EasyEffects preset write + preset load
```

Only content is corrected: the deviation is level-normalized, so volume
changes and master loudness never distort the result. Your own IEM corrections
stay on top through `eq.base_curve`, so the adaptive layer fixes the track,
not your headphones.

New tracks switch at once, and in-track refinement is slow and switched in at quiet moments.

The track's own spectral tilt is fitted with a robust line (median of pairwise slopes) and left alone. Only bumps and dips beyond `deadzone_db`, capped at `residual_cap_db`, are corrected, scaled by `strength` and classifier confidence. The device curve is added in full afterwards.

### How the spectrum is measured

- Each analysis frame's 1/3-octave levels are normalized by that frame's own overall level, so playback volume does not change the result.
- Frames more than 10 dB below the median frame level are ignored. The median (not mean power) is used so a short loud burst cannot gate out the quiet majority.
- The shape is the per-band median over the last `shape_window_s` seconds, so loud passages do not decide it.
- Reliability is the split-half agreement. The first and second half of the window are measured separately: 1.0 means they agree, 0 means they differ by 6 dB rms or more. It scales the correction strength (`strength x (0.5 + 0.5 x reliability)`) and is shown in the dashboard. It says how repeatable the measurement is, not whether the result sounds right.
- A transient-energy fraction (the share of band energy that is a short excess over each band's own 9-frame temporal median) is used as a proxy for percussive material. Percussive material is expected to be higher in bass and treble, so those corrections are reduced by `percussive_relief`. The mapping from the fraction to 0..1 (0.15 to 0.45) has not been calibrated against real music. The fraction and reliability are written to `tracks.jsonl` (version 2 rows) so it can be.
- The archetype label is decided once from the first window and is not changed during the track.

## Device profile

Run `leungo device <files>` with one or more AutoEq `ParametricEQ.txt`
files of your headphones or IEMs. Several sources are averaged, so one unit or
coupler does not decide the result. The averaged curve is fitted onto the 8
bands and written to `eq.device_curve`. The largest gain is shifted to 0 dB
(the printed "level offset") so it cannot clip. Eight broad bands cannot
reproduce narrow high-Q notches; the printed fit error shows how close the fit
gets. `eq.device_strength` (default 0.6) scales how much of that curve is
applied.

Getting the files: browse
<https://github.com/jaakkopasanen/AutoEq/tree/master/results>, open the
source, then the form factor folder (`in-ear`, `over-ear`), then your model,
and download `<model> ParametricEQ.txt`. Download the same model from two or
three sources if they exist, then:

```sh
leungo device ~/Downloads/*ParametricEQ.txt
```

The repository does not ship any profile data. AutoEq is MIT licensed; the
licensing of the underlying measurement data was not checked.

## Installation

| Platform | State |
|---|---|
| Linux (PipeWire or PulseAudio) | Works. Tested on Fedora with PipeWire and EasyEffects 8. |
| macOS | Not working yet, see below. |

### Linux

You need:

- PipeWire with `pipewire-pulse`, or PulseAudio. Capture uses the PulseAudio
  API, so both work.
- EasyEffects 7 or 8 with the `easyeffects` command on `PATH`. The distribution
  package is what was tested; the Flatpak build was not.
- A Rust toolchain, 1.85 or newer (the workspace uses edition 2024), and a C
  linker (`gcc` or `clang`).
- On Debian or Ubuntu the PulseAudio development package (`libpulse-dev`) may
  be needed to link. This was not tested; on the Fedora machine used for
  development the build needed no extra packages.

Build and install:

```sh
git clone https://github.com/rizqyep/leungo.git
cd leungo
cargo build --release -p leungo-cli
install -Dm755 target/release/leungo ~/.local/bin/leungo
```

Make sure `~/.local/bin` is on your `PATH`, then:

1. Start EasyEffects (service or background mode is fine) and play something.
2. Run `leungo gui`. The dashboard opens at `http://127.0.0.1:8431`.
3. Optional: set your headphone profile, see "Device profile", and pick a
   tuning preset in the dashboard.

The config is created on first run; `leungo config-path` prints where.
There is no autostart unit yet, so start the daemon yourself or from your
desktop's startup applications.

### macOS

macOS is not supported yet, and building there will not give you a working
equalizer today. Two parts of the program are Linux-only: the capture uses the
PulseAudio API and the EQ is applied through EasyEffects. Whether the workspace
even compiles on macOS has not been tested.

The planned design, not yet implemented: capture from the BlackHole virtual
device (`brew install blackhole-2ch`), apply the 8 filters in-process, and play
to the real output; track hints would come from AppleScript polling of Spotify
and Music. If you want to follow or help with that, it is item 2 in the
roadmap.

## Usage

```sh
# daemon + browser GUI (opens http://127.0.0.1:8431)
leungo gui

# headless daemon
leungo run

# in another terminal
leungo status          # state, archetype, confidence, current EQ
leungo bypass on|off   # pause adaptation, restores the previous curve

# import your existing manual EQ as the base curve
leungo calibrate <preset-name-or-path>

# analyze a 48 kHz WAV offline, no playback needed
leungo analyze song.wav

leungo config-path     # where the config lives
```

The GUI is a local web page served by the daemon itself (no external
resources, nothing leaves the machine). It shows the live state, the playing
spectrum, the applied EQ response, and has a bypass toggle. Close the tab any
time; the daemon keeps adapting.

Start EasyEffects first (service mode is fine). The daemon writes the preset
`~/.local/share/easyeffects/output/leungo.json` and loads it whenever a
new track is classified. The preset carries both the EasyEffects 7 (`equalizer`)
and 8 (`equalizer#0`) setting keys, so one file works across versions.

## Config

`~/.config/leungo/config.toml` is created with defaults on first run.

```toml
[analysis]
window_s = 6.0            # audio analyzed per track before classifying
gap_s = 1.2               # silence gap that counts as a track change
silence_threshold_db = -60.0
minimum_change_db = 1.0   # skip refinement steps smaller than this
defer_max_s = 10.0        # wait up to this long for a quiet moment before switching, 0 = at once
defer_dip_db = 4.0        # a frame this far below the recent level is a quiet moment
shape_window_s = 20.0     # seconds of recent audio the spectral shape is measured over (median across frames)
refine_interval_s = 5.0   # seconds between slow refinement passes while a track plays, 0 = off
refine_alpha = 0.35       # fraction of the remaining distance taken per pass
refine_max_step_db = 1.5  # largest per-band change per pass

[eq]
frequencies = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0]
q = 1.41
clamp_db = 12.0
strength = 0.5             # fraction of the deviation corrected, 0..1
base_curve = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]  # your manual IEM EQ
device_curve = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]  # fixed headphone/IEM correction, applied in full; written by `leungo device`
deadzone_db = 2.0          # deviations from the track's own tilt smaller than this are left alone
residual_cap_db = 6.0      # largest deviation the adaptive layer will chase
percussive_relief = 0.5   # fraction the bass/treble correction is reduced for fully percussive material, 0 = off

[targets]                  # dB deltas per archetype, at eq.frequencies
bass_heavy     = [1.5, 1.5, 0.5, 0.0, 0.0, -0.5, -1.0, -0.5]
vocal_centric  = [0.0, 0.0, 0.3, 0.8, 0.8, 0.3, 0.0, 0.0]
guitar_band    = [0.5, 0.5, 0.8, 0.8, 0.8, 0.3, -1.0, -0.5]
bright_acoustic= [0.0, 0.0, 0.0, 0.0, 0.0, -0.5, -1.0, -1.0]
neutral        = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
```

Tuning tips:

- If the adaptive layer fights your taste, adjust `targets`: positive values
  let that archetype keep more energy in a region, negative values lean
  against it. Positive bass values on `bass_heavy` mean "let EDM keep its
  low end", not "add bass".
- Run `calibrate` once with the preset you already use, so your IEM
  corrections are baked into `base_curve`.
- `strength` is the main taste knob. 0.3-0.5 keeps most of the mastering
  intent, 1.0 flattens every track toward the target. Low classifier
  confidence shrinks it further.
- In-track curve changes are held until a dip in level (a rest, a breath, a
  phrase gap) so the switch is hidden, or until `defer_max_s` passes. New
  tracks apply immediately.
- A new track (next or previous in the player) is always analyzed from scratch
  and its curve is applied at once. Pausing and resuming the same track does
  nothing.
- While a track plays the curve is refined slowly: small steps every
  `refine_interval_s`, only when the change is audible (`minimum_change_db`),
  and switched in at a quiet moment (`defer_dip_db`, `defer_max_s`).
- Capture reads `easyeffects_sink.monitor` (the signal before the EQ) when
  EasyEffects is running, so the analysis never measures its own correction.
- Confidence is a margin between the two closest archetypes and is usually low
  with the built-in prototypes. Run `leungo fit` after about 30 tracks to
  fit prototypes to your own listening. It prints the mean confidence before
  and after. The number shows how typical a track is of its group, not whether
  the group is right.

## Tuning presets

Presets bundle eight settings: `eq.strength`, `eq.deadzone_db`, `eq.residual_cap_db`, `analysis.refine_interval_s`, `analysis.refine_alpha`, `analysis.refine_max_step_db`, `analysis.defer_max_s`, `analysis.shape_window_s`. The device curve, targets and capture settings are never changed.

| preset | strength | dead zone dB | cap dB | refine every s | alpha | max step dB | defer max s | shape window s |
|---|---|---|---|---|---|---|---|---|
| conservative | 0.35 | 3.0 | 4.0 | 8 | 0.25 | 1.0 | 12 | 30 |
| balanced | 0.5 | 2.0 | 6.0 | 5 | 0.35 | 1.5 | 10 | 20 |
| aggressive | 0.8 | 1.0 | 8.0 | 2 | 0.5 | 2.5 | 6 | 10 |

Choose a preset in the dashboard dropdown or run `leungo tuning conservative|balanced|aggressive|custom`. With no argument, the command prints the current preset. A running daemon applies the change within about a second and re-solves immediately. `custom` uses the values written in the config file. A `tuning = "balanced"` line at the top of the config sets the default preset.

The values are starting points that have not been tuned by listening.

## Fitting to your listening

- The daemon logs descriptors of each analyzed track to `~/.local/share/leungo/tracks.jsonl`.
- `leungo fit` needs at least 30 tracks in two or more groups.
- It writes `~/.config/leungo/classifier.json`.
- Restart the daemon to use it.

## Architecture

```
crates/
  leungo-dsp               RBJ biquads, magnitude response, DF-I state
  leungo-eq                archetypes, target curves, least-squares EQ solver
  leungo-analysis          FFT spectrum, features, track detector, classifier
  leungo-core              config, engine state machine, traits, offline analysis
  leungo-capture-pipewire  monitor capture via the PulseAudio API
  leungo-mpris             track changes and now-playing from MPRIS players
  leungo-applier-ee        EasyEffects preset writer/loader + bypass
  leungo-cli               the leungo binary
```

The capture and applier traits (`SampleSource`, `EqApplier`) are the platform
seams: a Windows build swaps in a WASAPI loopback source and an Equalizer APO
writer without touching the engine.

## Known limitations

- 2048-point FFT at 48 kHz puts the effective analysis floor at ~50 Hz; deeper
  bass corrections come from the 63/125 Hz bands.
- Track changes come from the player (MPRIS) when available, otherwise from
  silence gaps. A player without metadata that crossfades gaplessly may not be
  detected. Continuous mixes with 30+ second gaps between changes work; shorter
  crossfades may skip a re-analysis (by design, to avoid false triggers from
  notifications).
- The classifier is rule-based on spectral signatures. It is transparent and
  fast, but the guitar-band vs neutral boundary is fuzzy; an ML classifier is
  a later upgrade (see roadmap).
- WAV analysis expects 48 kHz; resample other rates first.

## Roadmap

1. Windows: WASAPI loopback capture + Equalizer APO config writer
2. macOS: CoreAudio tap capture + BlackHole/eqMac applier
3. ML archetype classifier (ONNX runtime, distilled model)
4. Library pre-scan cache for instant presets on known tracks
5. Native window front end (egui) beside the web GUI; needs GUI dev headers
   (wayland-client, libxkbcommon, gtk3) that are not currently installed here
6. Optional standalone Linux mode: PipeWire virtual sink with in-process DSP

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
