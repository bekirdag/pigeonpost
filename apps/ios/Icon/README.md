# The app icon, and where it came from

`Pigeonpost/Assets.xcassets/AppIcon.appiconset/icon-1024.png` — 1024×1024, sRGB, **no alpha**, which
is what the App Store requires and what the asset catalog is checked for.

The only artwork in this repository is raster: `assets/img/logo_only_symbol.png` is 800×533, and the
mark inside it is about 666×545. An app icon is 1024×1024, so using that file directly would mean
upscaling by 1.7× — a soft icon at the one size everybody sees the product at.

There is no vector original here, so the mark was **traced back into vector** rather than enlarged:

```
magick assets/img/logo_only_symbol.png -background white -alpha remove -alpha off \
  -filter Lanczos -resize 200% -depth 8 logo2x.ppm
python3 Icon/trace.py logo2x.ppm traced.json     # classify, marching squares, simplify
python3 Icon/emit.py                             # centre on the ink, emit the SVGs
magick -background none icon-light.svg -alpha off -type TrueColor -depth 8 icon-1024.png
```

`trace.py` classifies every pixel to the nearest of the artwork's four colours — ground `#FFFFFF`,
navy `#05368B`, blue `#086CFF`, green `#26CE2D` — walks each region's boundary with marching squares,
and simplifies each contour with Douglas-Peucker. The tolerance scales with the shape: one that is
invisible on a 600px bird outline turns a 20px dot inside a speech bubble into a visible octagon, and
those dots are the part that says "message". Holes — the eye, the six dots — come out as their own
loops and are filled `evenodd`.

`pigeonpost-mark-light.svg` and `pigeonpost-mark-dark.svg` are the output, and are the closest thing
to a vector source this repository has. **They are a reconstruction, not the designer's original.**
If the original vector ever turns up, prefer it and delete these.

Two things deliberately not done:

- **The dark and tinted iOS 18 variants.** `pigeonpost-mark-dark.svg` is ready — navy ground, the
  bird reversed to white, the bubbles keeping their colours — but those variants want a transparent
  background rather than an opaque one, which is the opposite of the rule the App Store icon follows.
  Worth adding as its own change, with the transparency checked at the sizes it actually appears at.
- **Matching the UI palette.** The mark's colours are its own and more saturated than the app's
  tokens in `Design/Theme.swift` (`#16326B`, `#2563EB`, `#22C55E`). The logo is the logo; the icon
  keeps the logo's colours.
