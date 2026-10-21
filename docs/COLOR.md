# Colour, gradients, and LUTs

These operations are additive Fastpkg APIs. They are implemented as native pipeline ops, not JavaScript loops or wrappers around several existing operations.

All colour arguments accept a CSS-style string or `{ r, g, b, alpha }` / `{ r, g, b, a }`.

## `preserveOpacity`

`preserveOpacity` defaults to `true` for the operations that can change alpha:

- `replaceColor`
- `recolor`
- `duotone`
- `gradientMap`
- `gradientRecolor`
- `gradientFill`
- `fill`
- `paletteSwap`

With `preserveOpacity: true`, the input alpha channel is left untouched. With `false`, the operation writes the target/palette alpha.

## Point colour operations

```js
image('in.png')
  .replaceColor('#ff00ff', '#00ffff', {
    tolerance: 0.12,
    softness: 0.08,
    preserveOpacity: true,
  })
  .recolor('#2563eb', {
    amount: 0.8,
    preserveOpacity: true,
  })
  .toFile('out.png');
```

### `replaceColor(from, to, options)`

Replaces pixels close to `from` with `to`.

- `tolerance`: `0..1` (fraction of maximum RGB distance) or `0..441` raw distance.
- `softness`: feathering outside the tolerance.
- `amount`: blend strength.
- Uses RGB Euclidean distance; it does not alter alpha unless `preserveOpacity` is false.

### `recolor(color, options)`

Replaces chroma in OKLab while retaining each pixel's lightness. This produces a monochromatic recolour that preserves detail instead of a flat fill.

- `amount`: chroma replacement strength.
- Target alpha is used only when `preserveOpacity` is false.

### `fill(color, options)`

Replaces RGB with a solid colour while optionally retaining alpha. If the first argument is a gradient, it delegates to `gradientFill`.

## Duotone and gradient maps

```js
image('in.png')
  .duotone('#0b1020', '#f6d365', { preserveOpacity: true })
  .toFile('duotone.png');
```

`duotone(shadows, highlights, options)` is a two-stop luminance gradient map.

```js
image('in.png')
  .gradientMap([
    [0, '#000000'],
    [0.45, '#ff006e'],
    [1, '#ffbe0b'],
  ])
  .toFile('mapped.png');
```

`gradientMap` maps each pixel's luminance through a gradient. `gradientRecolor` is the same luminance mapping with a different name.

## Spatial gradients

```js
const gradient = image
  .gradient()
  .linear(45)
  .spread('reflect')
  .addStop(0, '#0f172a')
  .addStop(0.5, '#7c3aed')
  .addStop(1, '#f8fafc');

image('in.png')
  .gradientFill(gradient, { preserveOpacity: true })
  .toFile('gradient.png');
```

Gradient builder:

```js
const radial = image
  .gradient()
  .radial(0.5, 0.5, 0.6)
  .spread('pad')
  .addStop(0, '#ffffff')
  .addStop(1, '#000000');
```

Supported gradient features:

- Types: `linear`, `radial`
- Linear angle in degrees
- Radial centre and radius
- Spread modes: `pad`, `repeat`, `reflect`
- Any number of stops
- Stops as `{ offset, color }` or `[offset, color]`

`image.linearGradient(angle)` and `image.radialGradient(cx, cy, radius)` are shortcuts.

## Curves and levels

```js
image('in.png')
  .curves({
    rgb: [[0, 0], [0.25, 0.35], [0.75, 0.82], [1, 1]],
    r: [[0, 0], [0.5, 0.58], [1, 1]],
  })
  .levels({
    inputBlack: 12,
    inputWhite: 242,
    gamma: 1.05,
    outputBlack: 0,
    outputWhite: 255,
  })
  .toFile('graded.png');
```

Curves use monotone cubic interpolation over control points. Points may be `0..1` or `0..255`; `curves` normalizes them. A master/RGB curve is applied before per-channel curves.

Levels accept scalars or per-channel arrays for `inputBlack`, `inputWhite`, `gamma`, `outputBlack`, and `outputWhite`.

## 3D LUTs

```js
image('in.png')
  .applyLut('film.cube', {
    strength: 0.85,
    interpolation: 'trilinear',
  })
  .toFile('graded.png');
```

- Reads Adobe `.cube` 3D LUT files.
- Accepts a path, a `Buffer`, or LUT text directly.
- Supports `DOMAIN_MIN` / `DOMAIN_MAX`.
- `strength` blends between the original and LUT output.
- `interpolation`: `trilinear` or `nearest`.
- 1D `.cube` LUTs are not supported yet; use a 3D LUT.

## White balance and colour balance

```js
image('in.png')
  .whiteBalance({ temperature: 18, tint: -4 })
  .colourBalance({
    shadows: [-8, 0, 12],
    midtones: [0, 4, 0],
    highlights: [10, 0, -10],
  })
  .toFile('balanced.png');
```

- `whiteBalance`: `temperature` and `tint` are `-100..100`; positive temperature is warmer, positive tint is magenta.
- `colourBalance`: per-range additive shifts, `-100..100` per channel.

## Posterize, solarize, palette swap

```js
image('in.png')
  .posterize(6)
  .solarize(140)
  .paletteSwap(['#0b1020', '#3b82f6', '#f8fafc'], { dither: 0.15 })
  .toFile('stylized.png');
```

- `posterize(levels)`: quantizes RGB to `levels` steps, `2..256`.
- `solarize(threshold)`: inverts channels above the threshold.
- `paletteSwap(palette, { dither })`: nearest-colour mapping with optional 8×8 Bayer dithering.

## Alpha from luminance

```js
image('in.png')
  .alphaFromLuma({
    channel: 'luma',
    invert: false,
    multiply: false,
    scale: 1,
    offset: 0,
  })
  .toFile('mask.png');
```

`channel` can be `luma`, `r`/`red`, `g`/`green`, `b`/`blue`, or `a`/`alpha`. `multiply` multiplies the generated value into the existing alpha instead of replacing it.

## Performance

All operations run in the native pipeline:

- Point operations and gradient LUTs are parallelized with rayon over pixel rows.
- Gradient maps are reduced to a 256-entry lookup table.
- Spatial gradients use a 1024-entry cached gradient LUT.
- Palette mapping and LUT filtering run in a single pass.
- Alpha-preserving operations avoid allocating a second image.
