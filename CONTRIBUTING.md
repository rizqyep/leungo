# Contributing to Leungo

Thanks for taking a look. This file covers how to build, test and change the
project. For how it works inside, read [TECHNICAL.md](TECHNICAL.md) first.

## Before you start

- **Open an issue for anything non-trivial.** Especially changes to the sound
  (targets, thresholds, defaults), new platforms and new dependencies. Small
  fixes and docs can go straight to a pull request.
- **Sound changes need evidence.** Say what you measured or listened to, on
  what material and device, and what you could not check. "Sounds better to me"
  is useful when you say which tracks and which headphones. Defaults in the
  config are starting points, not tuned values, and TECHNICAL.md marks which
  are unverified.
- By contributing you agree your work is released under the repository's
  [MIT license](LICENSE).

## Setting up

You need Rust 1.85 or newer (the workspace uses edition 2024) and a C linker.

| Platform | Extra requirements |
|---|---|
| Linux | PipeWire with `pipewire-pulse` (or PulseAudio), and the PulseAudio client headers: `libpulse-dev` (Debian, Ubuntu) or `pulseaudio-libs-devel` (Fedora). EasyEffects 7 or 8 to run the app end to end. |
| macOS (Apple silicon) | Xcode command line tools. BlackHole 2ch to run it end to end. |

```sh
git clone https://github.com/rizqyep/leungo.git
cd leungo
cargo build --release -p leungo-cli
```

## The checks CI runs

Run these before you push. They are what the Linux and macOS jobs run.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Warnings are errors. If clippy complains about something that is only used on
one platform, gate it (see below) instead of adding a blanket `allow`.

## Running your change

The daemon owns the EasyEffects preset on Linux, and the system output route on
macOS, so **do not run two copies at once**. If you already have Leungo running,
stop it first. To try a build without opening a window:

```sh
./target/release/leungo gui --no-open --port 8499
```

Useful tools that need no audio playback:

```sh
leungo analyze song.wav      # offline analysis of a 48 kHz WAV, prints the curve
leungo status                # state of a running daemon
leungo config-path           # where the config lives
```

Back up `~/.config/leungo/config.toml` before experimenting. The config rejects
unknown keys on purpose, so a typo shows up as an error.

## Where things live

| You want to change | Look in |
|---|---|
| Filter math, shelves | `leungo-dsp` |
| Targets, the solver, device profiles, sound signature | `leungo-eq` |
| Measurement, track detection, the classifier | `leungo-analysis` |
| Engine behavior (when to analyze, refine, apply), config, presets | `leungo-core` |
| Linux capture, EasyEffects | `leungo-capture-pipewire`, `leungo-applier-ee` |
| Track ids and now-playing | `leungo-players` |
| The macOS engine | `leungo-standalone` |
| CLI, dashboard server and page, setup screens | `leungo-cli` (`gui.rs`, `gui.html`, `setup.rs`) |

## Platform-specific code

The workspace has to build and lint on Linux **and** macOS.

- Gate platform code with `#[cfg(target_os = "...")]` and keep the shared path
  free of platform imports. Platform behavior goes behind the `SampleSource`
  and `EqApplier` traits in `leungo-core`.
- Items used on only one platform need `#[cfg_attr(not(target_os = "macos"),
  allow(dead_code))]` (or the Linux equivalent), otherwise the other platform's
  clippy run fails.
- You may not be able to run the other platform locally. Push the branch and let
  CI check it; read the failure rather than guessing.
- Pure logic (resampling, drift control, filter chains, parsers) should be
  testable without audio hardware. The cpal code is compiled in CI but not run
  against a device. Say so in your PR if your change touches it.

## Tests

- Put unit tests next to the code. Use synthetic signals (tones, noise,
  splices) so tests are deterministic and need no audio files.
- Engine behavior goes in `crates/leungo-core/tests/engine.rs`, which drives the
  engine with synthetic audio and a recording applier.
- If a test fails, work out whether the test or the code is wrong before
  changing either. Several bugs here were found by a test disagreeing with a
  plausible-looking fix.
- A fix for a bug should come with a test that fails without it.

## Code style

- Follow the code around you: naming, comment density, error handling.
  `cargo fmt` is the formatter.
- Comments explain *why*, especially for thresholds and for things that were
  tried and did not work.
- Do not add a new config key without documenting it in the README's config
  section, and without a default that keeps existing behavior.
- Prefer small, bounded corrections over aggressive ones. The design goal is an
  EQ that never makes a track worse, so new behavior should have a bound.

## Commits and pull requests

- One logical change per commit, with a message that says what changed and why.
- In the PR description, state: what you changed, how you checked it, and what
  you did **not** verify (a platform you could not run, music you did not try).
- Keep unrelated formatting or refactoring out of functional changes.

## Reporting a problem

Include:

- Operating system and version, and how you installed Leungo (package, dmg,
  source).
- On Linux: the output of `leungo setup`, whether EasyEffects is running, and
  `pactl list short sources | grep -i easyeffects`.
- On macOS: the output of `leungo devices`, and the first lines the daemon
  printed.
- `~/.cache/leungo/status.json` (state, archetype, reliability) if it is about
  the sound.
- What you expected, and what happened instead. "The EQ sounds off" is more
  useful with the song, the headphones, and which direction (too much bass,
  harsh, dull).

## Releases

Maintainers cut a release by pushing a version tag:

```sh
git tag v0.x.y && git push origin v0.x.y
```

The `release` workflow then builds the macOS `.dmg` and the Linux `.deb` and
`.rpm`, installs the packages as a smoke test, and publishes them. To check
packaging without publishing, run the workflow manually from the Actions tab
(`workflow_dispatch`); the publish job only runs for tags.

## Data you should not add

The headphone profile files under `profiles/` are not in the repository because
the licensing of the underlying measurement data was not verified. Do not
commit third-party measurement data, music, or recordings without a clear
license.
