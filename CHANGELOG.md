# Changelog

Each release lists what got better. Builds are unsigned (macOS) and audio
has not been verified on all hardware; please report problems.

## v0.1.7

- Drum attack is now on by default and has a clearer description in the
  dashboard: it keeps the hit of kicks and snares while boomy bass lines
  and muddy low mids still get cleaned up. Turn it off for the plain
  correction.
- If your `config.toml` already says `per_band_punch = false` (for example
  after running `leungo device`), it stays off. Switch it on once in the
  dashboard's Sound card.

## v0.1.6

### Song analysis

- The 31.5, 40 and 80 Hz bands are now measured on their own. Before,
  they were skipped, so kick and bass around 80 Hz were only guessed from
  the neighbouring bands, and bass corrections really started at 50 Hz.

### Drum attack (test)

- New switch in the dashboard's Sound card. It keeps kick and snare hits
  from being trimmed while boomy bass lines and low mids still are, so
  you get the attack without a cloudy low end.
- It only acts on clear drum hits in the kick (40-125 Hz) and click/snare
  (2-5 kHz) ranges; 150-500 Hz is never spared.
- Flips live on the current song, so you can compare. Off by default.

## v0.1.5

### Headphone correction

- `leungo device` now takes AutoEq measurement CSVs (`frequency,raw`) and
  fits a correction toward Harman IE 2019 (or any target via `--target`).
  Several measurements of the same model are combined by median, and the
  correction backs off where they disagree.
- The headphone correction runs as its own stage with free frequency, Q and
  shelf filters instead of being squeezed into the 8 song bands. On a KZ EDX
  Pro: 0.36 dB rms error vs 1.12 dB before.
- Treble correction fades out from 8 to 12 kHz (fit and insertion depth
  matter more there than the measurement), boosts are capped at +6 dB and
  narrow dips are never filled.
- A preamp keeps the combined curve from ever going above 0 dB.
- Old ParametricEQ profiles still work.

### Song analysis

- Bass is measured on a separate fine-resolution path (2.9 Hz instead of
  23 Hz bins), so 63, 80 and 100 Hz no longer blur together.
- Fixed two measurement errors: band edges caused fake bumps of up to
  1.4 dB around 300-600 Hz, and the low bass bands read about 1.4 dB low.
  Pink noise now reads flat within 0.25 dB from 31.5 Hz to 16 kHz.
- Bass corrections now reach down to 40 Hz (was 50 Hz).
- Lossy streams: the codec's cutoff is detected and treble corrections stop
  below it.
- New, off by default: `eq.per_band_punch = true` judges drums per band, so
  a kick's punch is kept while a boomy bass line is still trimmed.

### Other

- `leungo analyze` is about 40x faster on long files.
- The track log records each track's band shape when it ends, for upcoming
  per-library tuning. It stays on your machine.
