# Leungo brand assets

*Leungo* is Acehnese for "listen". The mark is a lowercase **l** whose foot bends into an EQ curve, with two band handles.

## Files

| Folder | What | Use |
|---|---|---|
| `svg/leungo-icon.svg` | Master app icon, dark tile | Source for ≥48 px |
| `svg/leungo-icon-small.svg` | Heavier stroke, solid nodes | Source for ≤32 px (don't just downscale the master) |
| `svg/leungo-icon-light.svg` | Light tile | Light-theme contexts |
| `svg/leungo-mark.svg` / `leungo-mark-on-dark.svg` | Mark without tile, transparent | README, docs, website |
| `svg/leungo-mark-mono.svg` | Pure black, transparent | Tray / menu-bar icon (macOS template image) |
| `svg/leungo-lockup.svg` / `leungo-lockup-on-dark.svg` | Icon + wordmark | README header, website |
| `png/leungo-{16…1024}.png` | Rendered app icon | Linux hicolor, Tauri/Electron icon sets |
| `tray/leungo-tray-{16,22,32,44}.png` | Mono tray icon | 16/32 = macOS @1x/@2x, 22/44 = Linux/GNOME |
| `app/leungo.ico` | 16–256 px | Windows |
| `app/leungo.icns` | 16–1024 px | macOS |

## Colors

| Token | Hex |
|---|---|
| Ink | `#141614` |
| Paper | `#F6F7F2` |
| Saffron (accent on dark) | `#E39A2C` |
| Saffron deep (accent on light) | `#B5700B` |

## Wordmark

Lowercase `leungo`, Sora SemiBold (600), tracking −3%. The SVG lockups have the text outlined to paths, so they render without the font installed. Sora is licensed under the SIL Open Font License 1.1.

## Rules of thumb

- Keep clear space around the icon of at least ¼ of its width.
- Don't recolor the stroke; swap the accent only for the Ink/Paper pairs above.
- On macOS, set the tray PNG as a template image so it follows light/dark menu bars.
