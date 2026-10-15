<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/svg/leungo-lockup-on-dark.svg">
    <img alt="leungo" src="assets/brand/svg/leungo-lockup.svg" height="64">
  </picture>
</p>

Leungo is Acehnese for "listen".

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
| macOS (Apple silicon) | In-process DSP via BlackHole | Experimental: builds and passes tests in CI, audio not yet verified on hardware |

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
| macOS (Apple silicon) | Experimental, see below. |

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

If you used an earlier build called adaptive-eq, your config, track log and
settings are moved to the new `leungo` directories automatically the first
time you run `leungo`.

The config is created on first run; `leungo config-path` prints where.
There is no autostart unit yet, so start the daemon yourself or from your
desktop's startup applications.

### macOS (experimental)

What is verified: the code builds on an Apple silicon runner in CI, its
clippy and test suites pass there, and the resampling, drift control and
filter chain are covered by unit tests. What is not verified: real audio on
real hardware. Expect rough edges and please report them.

How it works there: there is no EasyEffects on macOS, so leungo captures a
virtual loopback device (BlackHole), equalizes in process, and plays the result
to your real output device. Track information comes from polling Spotify and
Music with AppleScript.

Prerequisites:

```sh
xcode-select --install                    # compiler and linker
curl https://sh.rustup.rs -sSf | sh       # Rust 1.85 or newer
brew install --cask blackhole-2ch         # the loopback device
```

Build and install:

```sh
git clone https://github.com/rizqyep/leungo.git
cd leungo
cargo build --release -p leungo-cli
mkdir -p ~/.local/bin
install -m755 target/release/leungo ~/.local/bin/leungo   # or any folder on PATH
```

Set it up:

1. In System Settings, Sound, choose **BlackHole 2ch** as the output. Everything
   you play now goes to BlackHole instead of your speakers.
2. Run `leungo devices` and note the exact name of your real output (the
   speakers or headphones you actually listen on).
3. Run `leungo config-path`, open that file, and set that name in the
   `[standalone]` section:

   ```toml
   [standalone]
   input_device = "BlackHole"
   output_device = "MacBook Pro Speakers"   # a part of the name is enough
   latency_ms = 60
   ```

   If `output_device` is empty, leungo plays to the system default output. That
   is BlackHole in step 1, which would feed back, so it refuses to start and
   lists your devices.
4. Run `leungo gui`.

Permissions macOS will ask for the first time:

- **Microphone**: capturing from an input device counts as microphone access.
  Allow it for the terminal you run `leungo` from.
- **Automation**: controlling Spotify or Music so track titles and track
  changes can be read. Without it leungo still works and falls back to
  detecting silence gaps between tracks.

Known limitations:

- With the output set to BlackHole the keyboard volume keys do not change the
  loudness. Use the volume control in your player. (BlackHole has no volume
  control of its own; this is how loopback devices behave, not verified here.)
- The processing adds about the configured `latency_ms` of delay. That is fine
  for music and noticeable on video.
- Only 32-bit float devices are supported, which is what CoreAudio normally
  offers.
- The two devices run on separate clocks. A small controller absorbs the
  difference; it is unit-tested against simulated drift but not yet heard on
  hardware, so occasional clicks are possible.
- A config from an earlier build lives under
  `~/Library/Application Support/leungo`.

If something fails, `leungo devices` and the first lines of the daemon output
are the most useful things to include in a report.

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

Other settings:

```toml
[gui]
port = 8431
open = "app"        # app: its own window, browser: a tab, none: print the address

[standalone]        # macOS only
input_device = "BlackHole"
output_device = ""  # your real output; must not be the loopback device
latency_ms = 60
```

`gui.open = "app"` uses a Chromium-family browser (Chrome, Chromium, Brave,
Edge) in app mode when one is installed: a window with no tabs or address
bar. It is still a browser process underneath, so it uses the browser's
memory while open. Closing the window does not stop the daemon.

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

## Sound

The dashboard has a Sound card with presets (Neutral, Punch & air, Warm,
Bright, V-shape), four controls and a device-correction slider.

- `bass`: -6 to +8 dB, low shelf below about 110 Hz.
- `warmth`: -4 to +4 dB, bump near 220 Hz.
- `presence`: -4 to +4 dB, bump near 3 kHz.
- `air`: -4 to +6 dB, high shelf from about 5.5 kHz. The highest of the 8
  bands is 8 kHz, so a shelf cannot start higher.
- Device correction: 0 to 100% (`eq.device_strength`, default 0.6). It scales
  how much of the headphone correction is applied.

The sound controls are applied on top of the correction and take effect
within about a second. They are shifted so the signature never adds a net
boost, so they add no extra clipping risk.

`leungo sound` prints the current values. `leungo sound --bass 4 --air 3
--device 0.6` sets them. Negative values work, for example `--warmth -2`.

The values are stored in the config under `[eq.signature]` as `bass_db`,
`warmth_db`, `presence_db` and `air_db`.

The dashboard tuning buttons are labelled Gentle, Balanced, Bold and Custom.
They map to the conservative, balanced, aggressive and custom presets.

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
  leungo-players           track changes and now-playing: MPRIS (Linux), AppleScript (macOS)
  leungo-standalone        macOS engine: loopback capture, resampling, in-process filters
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
2. macOS: verify the standalone engine on hardware, add a menu-bar item and a signed app bundle
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
