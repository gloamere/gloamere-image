# gloamere-image identity assets

Original vector artwork authored for this repository, 2026-10-09. No third-party
brand, stock artwork, icon font, or generated raster image is used. The source and
derived assets use the repository's MIT OR Apache-2.0 licensing.

- `gloamere-image.svg`: editable 64-unit master vector.
- `gloamere-image.ico`: nine 32-bit RGBA icon sizes for the main/worker EXEs,
  installer, shortcuts and file-association icon source.
- `gloamere-image-64.png`: embedded egui window icon.
- Other `gloamere-image-<size>.png` files: directly rendered per-size sources.
- `gloamere-image-preview.png`: light/dark inspection sheet, including actual 16px
  and a nearest-neighbor 6× detail view. Not embedded at runtime.

To regenerate, use Python 3.10+, Pillow and official Inkscape already installed in
your development environment:

```sh
python assets/generate_icons.py
python assets/generate_icons.py --check
```

Generation uses a temporary Inkscape profile and does not alter global settings.
The app build consumes committed images, requiring none of these rendering tools.
The recorded cloud rendering used Inkscape 1.4 and Pillow 12.3.0. Two consecutive
runs produced identical files. Different renderer versions may change edge
antialiasing; inspect the preview after intentional regeneration.

Sizes: 16, 20, 24, 32, 40, 48, 64, 128 and 256 pixels. Each size is independently
rasterized from the vector; the ICO preserves these exact RGBA pixels.

See `docs/branding-icons.md` for design rationale and the Windows test boundary.
