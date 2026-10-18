# Roadmap

Leungo's goal: the best sound your headphones can give, for every song, with
no manual EQ. Everything here is planned as signal processing and
statistics, not machine learning, so each step can be measured and checked.

Items are in rough priority order within each section. If you want to work
on one, open an issue first so we can agree on the approach. How the current
code works is in [TECHNICAL.md](TECHNICAL.md).

## Done

- Headphone correction from lab measurements: median consensus across
  sources, weighted by how much they agree, fitted with free filters (v0.1.5).
- Bass measured on a fine 2.9 Hz grid, and codec cutoff detection (v0.1.5,
  v0.1.6).
- Drum/tone separation (HPSS), so kick and snare attack is kept while boomy
  bass and mud are cut (v0.1.6, on by default since v0.1.7).
- Headphone search in the dashboard: download, fit and apply with no files or
  terminal (v0.1.8).
- Harman in-ear 2019 plus deep bass as the default target (v0.1.9).

## Next: judging changes fairly

1. **Blind, loudness-matched A/B in the dashboard.** Leungo's preamp makes
   "on" a bit quieter than "off", and louder always sounds better at first.
   A fair switch plays both at the same loudness (ITU-R BS.1770), hides which
   is which, and logs the choice. Every tuning decision below should be
   settled this way.
2. **Pick defaults from votes.** Win rates per setting with binomial
   confidence intervals, so a default changes only when listeners clearly
   prefer the new one.

## Headphones

1. **Per-output profiles.** Remember which headphones go with which output
   (Bluetooth address, USB DAC), and switch when the output changes.
2. **Automatic match by Bluetooth name**, with close-name suggestions when
   there's no exact match.
3. **Calibration by ear** for headphones nobody has measured. The dashboard
   plays narrowband noise at about 12 frequencies against a 1 kHz reference,
   the listener matches loudness, and the ISO 226:2023 equal-loudness contour
   is subtracted. Rougher than a lab measurement, but it captures your own
   fit.
4. **Category profiles** ("bass-boosted true wireless", "V-shaped IEM") as a
   low-strength fallback.
5. **ANC modes** for true wireless earbuds that measure differently with noise
   cancelling on and off.

## Songs

1. **Learn "normal" from the listener's own library.** Per band, the median
   and spread of the residual across logged tracks with a similar slope. A
   band is corrected by how unusual it is (a z-score), so bands that vary a
   lot between songs get a wide dead zone and stable bands a narrow one. A
   built-in default covers new installs until about 50 tracks are logged.
2. **Kick and bass separated in time.** A dynamic EQ band that trims bass
   sustain between kicks and lets each hit through, controlled by the drum
   share.
3. **Vocals from the stereo centre.** Mid/side analysis to tell a buried vocal
   (lift presence a little) from a dull mix (leave it).
4. **Masking model.** Specific loudness on an ERB scale (Glasberg and Moore)
   to find low mids masked by bass, where a small cut clears things up more
   than any boost.
5. **Musical key vs mix balance.** A note that's strong in every octave is the
   song's key, not a mix problem, and shouldn't be cut.
6. **Loudness compensation.** At low volume the ear loses bass and treble
   first (ISO 226). Optional, since it needs the playback level.

## Platforms and packaging

1. **macOS:** signed and notarized app, a menu bar item, and testing on more
   Macs and DACs.
2. **Windows:** an installer and signed builds, starting without a
   console window, and testing on more devices.
3. **CPU:** batch tiny capture packets and compute the drum split less often
   (currently about 1.3% of one core on Linux).

## Help wanted without code

- **Listening reports.** A song, your headphones, your settings, and what
  sounds wrong ("too much bass", "vocals scooped"). This is what the tuning
  numbers are judged on.
- **Measurements and devices.** Which headphones you use and whether search
  found them.
- **Testing on macOS**, especially other DACs and long sessions.
