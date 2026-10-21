# sharp compatibility

Status: **Supported**, **Partial**, **Unsupported**.

## Constructor and input

| sharp | Status | Notes |
|---|---|---|
| `sharp(input, options)` | Supported | String path, Buffer, Uint8Array, ArrayBuffer, stream, `create`, `text`, 8/16-bit `raw` |
| `clone()` | Supported | Copies the spec; shares the input handle |
| `metadata()` | Supported | Header-only |
| `stats()` | Supported | Full decode |
| `probe()` | Extension | Header-only fast path |
| `failOn` | Partial | Parsed; decoder-specific truncation policy is not fully wired |
| `limitInputPixels`, `unlimited` | Supported | Checked before decode |
| `sequentialRead` | Partial | Parsed; file input is mmap-based |
| `density`, `pages`, `page`, `animated` | Partial | Parsed; animation frame selection is not implemented |
| `autoOrient` input option | Supported | |
| `ignoreIcc` | Partial | Parsed; no colour transform is applied |

## Output

| sharp | Status | Notes |
|---|---|---|
| `toBuffer`, `toUint8Array`, `toFile` | Supported | Promise and callback forms; `resolveWithObject` |
| `toFormat` | Supported | |
| `jpeg`, `png`, `webp`, `avif`, `gif`, `qoi` | Supported | WebP supports lossy and lossless; GIF is single-frame |
| `jxl` | Optional | Decode always; encode requires the `jxl-encode` feature |
| `raw` | Supported | RGBA/RGB output; `depth: 'ushort'` emits 16-bit samples |
| `tile` | Partial | `dz`, `google`, `zoomify`, `iiif`, `iiif3`; filesystem or zip archive |
| `timeout` | Supported | Checked around decode, each operation, and encode |
| `keepExif`, `withExif`, `withExifMerge` | Partial | Raw EXIF preservation/injection for JPEG/PNG; `withExifMerge` still partial |
| `keepIccProfile`, `withIccProfile` | Partial | JPEG/PNG/WebP ICC preservation and raw injection |
| `keepXmp`, `withXmp` | Partial | JPEG/PNG XMP preservation and injection |
| `keepGainMap`, `withGainMap` | Unsupported | Parsed but not implemented |
| `keepMetadata`, `withMetadata` | Partial | Orientation via JPEG is supported |
| `withDensity` | Supported | JPEG JFIF and PNG pHYs density |
| `heif`, `tiff`, `jp2` | Unsupported | Throws explicit errors |

## Resize and geometry

`resize`, `extend`, `extract`, `trim`, `rotate`, `autoOrient`, `flip`, `flop`, `affine` are supported. `fastShrinkOnLoad` is honoured for JPEG DCT-scaled decode. `affine` uses bilinear sampling and supports a 2x2 or 2x3 matrix. Approximate kernels: `lanczos2` and `mks2013`/`mks2021` map to Lanczos3.

## Operations

| Operation | Status | Notes |
|---|---|---|
| `sharpen` | Supported | Parameter-aware unsharp mask; approximates sharp's LAB response |
| `median` | Supported | Square window |
| `blur` | Supported | Separable Gaussian; `sigma` only |
| `dilate`, `erode` | Supported | Square window |
| `flatten`, `unflatten` | Supported | |
| `gamma`, `negate`, `linear`, `recomb` | Supported | |
| `normalise`/`normalize` | Supported | Histogram percentile stretch |
| `clahe` | Supported | Tile histogram equalization with clip limit and bilinear tile interpolation |
| `convolve` | Supported | |
| `threshold`, `boolean` | Supported | |
| `modulate`, `tint`, `greyscale`/`grayscale` | Supported | |
| `composite` | Supported | Porter-Duff modes plus separable blend modes; one image per op |
| `removeAlpha`, `ensureAlpha`, `extractChannel`, `joinChannel`, `bandbool` | Supported | |
| `joinChannel` | Partial | Joins the first channel of the operand as alpha |

## Colour and channel

`toColourspace` supports `srgb`, `rgb`, `rgb16`/`rgba16`, `grey16`/`gray16`, and true one-channel `b-w`/`grey`. `pipelineColourspace` supports `srgb`, `rgb`, `linear`, `scrgb`, and `b-w`. CMYK/Lab/XYZ are still unsupported.

16-bit PNG decode/encode and raw `ushort` input/output preserve wide RGBA samples. Geometry operations (`resize`, `extract`, `extend`, `rotate`, `affine`, `flip`, `flop`, `autoOrient`) keep the wide buffer; point and filter operations currently run on the 8-bit representation and discard it.

## Utilities

`cache`, `concurrency`, `counters`, `simd`, `format`, `versions`, `interpolators`, `queue`, `block`, `unblock` are present. `cache` is a compatibility shim; `simd(false)` toggles the reported flag but does not disable compiled kernels; `queue` emits depth changes; `block`/`unblock` are no-ops.

## Extensions

- `probe()` header-only metadata
- `responsive(widths, { formats })` multi-variant helper
- `dominant()` convenience wrapper around `stats`
- `joinChannel` alpha-mask helper
- `tile({ layout: 'dz' | 'google' | 'zoomify' | 'iiif' | 'iiif3' })` pyramid output to a directory or zip archive
- `png({ palette: true, colors, dither })` indexed PNG output
- `toStream()` readable convenience
- `replaceColor`, `recolor`, `duotone`, `gradientMap`, `gradientRecolor`, `gradientFill`
- `applyLut`, `curves`, `levels`, `whiteBalance`, `colourBalance`/`colorBalance`
- `posterize`, `solarize`, `fill`, `paletteSwap`, `alphaFromLuma`
- Gradient builder with linear/radial types, arbitrary stops, and spread modes

## Explicitly excluded formats

TIFF, JPEG 2000/JP2, HEIC/HEIF, PDF. These are rejected with `UnsupportedFormatError` or a JavaScript error. AVIF is supported because it is a modern AV1-based format.
