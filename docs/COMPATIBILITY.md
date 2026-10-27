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
| `failOn` | Supported | Validated like sharp and honoured by the JPEG, PNG, WebP, GIF and AVIF decoders; checked against sharp on a corrupt-file corpus (`tests/failon.test.mjs`). Known differences: damaged AV1 payloads in AVIF and some damaged lossless WebP streams |
| `limitInputPixels`, `unlimited` | Supported | Checked before decode |
| `sequentialRead` | Partial | Parsed; file input is mmap-based |
| `density`, `pages`, `page`, `animated` | Partial | Parsed; animation frame selection is not implemented |
| `autoOrient` input option | Supported | |
| `ignoreIcc` | Supported | Embedded ICC profiles (RGB, grey, CMYK JPEG) are converted to sRGB at decode unless ignored, like sharp |

## Output

| sharp | Status | Notes |
|---|---|---|
| `toBuffer`, `toUint8Array`, `toFile` | Supported | Promise and callback forms; `resolveWithObject` |
| `toFormat` | Supported | |
| `jpeg`, `png`, `webp`, `avif`, `gif`, `qoi` | Supported | WebP supports lossy and lossless; GIF is single-frame |
| `jpeg({ parallel: true })` | Extension | Not a sharp option. Opt-in banded baseline encode for images of 1 MP or more (restart markers join the bands; decoded pixels equal a single-pass `optimiseCoding: false` encode). Ignores `optimiseCoding`, throws a `TypeError` with `progressive`/`mozjpeg`/`optimiseScans`, falls back to the normal encoder for small images or a single thread. See the README. |
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

`resize`, `extend`, `extract`, `trim`, `rotate`, `autoOrient`, `flip`, `flop`, `affine` are supported.

Operations run in sharp's fixed pipeline order, not call order (`packages/image/lib.cjs`, `normalizeOperations`): `rotate`/`flip`/`flop` run before `resize` only when `rotate(angle)` was called before `resize()`/`trim()`/`extract()` (sharp's `rotateBefore`), otherwise after it; `extract()` called before `resize()` is a pre-resize crop, after it a post-resize crop; `extend`, `affine`, `blur`, `sharpen`, `composite`, `negate`, ... always run after `resize` in sharp's order. The `cover` crop and `contain` padding happen after the flips/quarter turns that follow the resize, as in libvips. `withoutEnlargement`/`withoutReduction` clamp each axis' shrink factor (when both are set only `withoutReduction` applies), `fit: 'fill'` with one dimension keeps the other axis, and `fit: 'contain'` pads to the requested size with `background` (adding alpha for translucent backgrounds). `position` accepts names, `sharp.gravity` numbers, and `sharp.strategy.entropy`/`attention`; the content-aware strategies currently crop around the centre. `rotate(angle)` and `affine` size and place the output like libvips (rounded bounding box, bilinear sampling; pixels at the border blend into `background` by coverage, images with alpha are interpolated premultiplied). `rotate()`/`autoOrient()` reset a retained EXIF Orientation to 1, and `withMetadata({ orientation, density })` overrides the retained EXIF/JFIF values. JPEG shrink-on-load is sharp's: a DCT scale of 1/2, 1/4 or 1/8 only (the largest that leaves the resampler a factor of one, or two with `fastShrinkOnLoad: false`, lowered once more when it equals the integer part of the shrink factor), the partial last block of the shrunk image is dropped as libvips does, and the output size is derived from that shrunk image, so sizes are sharp's to the pixel. libjpeg can scale in eighths (3/8, 5/8, ...), which would decode faster for shrink factors between the powers of two, but it leaves the resampler a different residual than libvips and cost 10-25 dB of PSNR against sharp (worst case 42 dB for photographs and 27 dB for hard edges in 0.5.0; 53-56 dB and above now), so it is not used by default; `FASTPKG_JPEG_SHRINK=eighths` in the environment opts back in (about 20% faster on 3000x2000 JPEGs at 1.3x to 3x, at the old PSNR). Shrink-on-load is skipped, as in sharp, when `pipelineColourspace` is set. `affine` supports a 2x2 or 2x3 matrix and the `bilinear` (default), `bicubic` (Catmull-Rom) and `nearest` interpolators; `nohalo`, `lbb` and `vsqbs` fall back to bilinear. Resize kernels `nearest`, `linear`, `cubic`, `mitchell`, `lanczos2`, `lanczos3`, `mks2013` and `mks2021` use their own filter functions. Downscales follow `vips_resize`: libvips is given the shrink factors, not the rounded output size, so the kernel samples a region `out * shrink` wide centred on the image (a rounded-up output size makes it run past the edges, which are repeated), after a box shrink by `floor(in / out / 2)` when that is 2 or more (so the kernel sees at most twice the output). The same is done here when the source was not already shrunk at decode (a leading `rotate(90/180/270)`, PNG pre-shrink and scaled WebP decode keep the older ratio-based single pass), and on non-`nearest` kernels. What still differs: libvips rounds to 8 bits between its vertical and horizontal pass and uses 8-bit fixed-point coefficients, and the resampler here renormalises the kernel at the image border instead of repeating the edge pixel, so `resize()` agrees with sharp to 56-77 dB PSNR on photographs and 58-67 dB on hard edges (53-70 dB after `rotate()`). `rotate(90/180/270)` before `resize()` is the one JPEG pipeline where sharp decodes at full size (its `rotateBefore`); the engine still looks through the quarter turn and shrinks on load for speed and memory, which lowers the PSNR against sharp there to 35-60 dB (the size is still sharp's). libvips upscales with a bicubic interpolator whatever the kernel.

## Operations

| Operation | Status | Notes |
|---|---|---|
| `sharpen` | Supported | `sharpen()` is sharp's 3x3 convolution (bit-exact); `sharpen({ sigma, m1, m2, x1, y2, y3 })` runs libvips' LAB-lightness unsharp mask with its integer Gaussian mask and two-slope curve (within one grey level) |
| `median` | Supported | Square window |
| `blur` | Supported | Bit-exact libvips `gaussblur` for `precision: 'integer'` (default) and `minAmplitude`; `'float'` is within one level on ~0.05% of pixels; `'approximate'` uses the integer algorithm (up to 3 levels apart). `blur()` / `blur(true)` is sharp's 3x3 box filter. RGBA is blurred premultiplied |
| `dilate`, `erode` | Supported | Square window (`2 * width + 1`), alpha untouched |
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

`toColourspace` supports `srgb`, `rgb`, `rgb16`/`rgba16`, `grey16`/`gray16`, and true one-channel `b-w`/`grey`. `pipelineColourspace` supports `srgb`, `rgb`, `linear`, `scrgb`, and `b-w`. Lab/XYZ are still unsupported, and CMYK is input-only (CMYK JPEGs are converted to sRGB).

16-bit PNG decode/encode and raw `ushort` input/output preserve wide RGBA samples. Geometry operations (`resize`, `extract`, `extend`, `rotate`, `affine`, `flip`, `flop`, `autoOrient`) keep the wide buffer; point and filter operations currently run on the 8-bit representation and discard it.

## Utilities

`cache`, `concurrency`, `counters`, `simd`, `format`, `versions`, `interpolators`, `queue`, `block`, `unblock` are present. `cache` is a compatibility shim; `simd(false)` toggles the reported flag but does not disable compiled kernels; `concurrency(n)` is the number of threads each image job uses (changing it applies to the next job); `queue` emits depth changes; `block`/`unblock` are no-ops.

## Extensions

- `probe()` header-only metadata
- `responsive(widths, { formats })` multi-variant helper
- `dominant()` convenience wrapper around `stats`
- `joinChannel` alpha-mask helper
- `tile({ layout: 'dz' | 'google' | 'zoomify' | 'iiif' | 'iiif3' })` pyramid output to a directory or zip archive
- `png({ palette, quality, effort, colours, dither, compressionLevel, adaptiveFiltering, progressive })` PNG output; indexed output uses the built-in quantiser (see the README, "PNG output"). Bytes are stable within a release line but not libvips-identical.
- `toStream()` readable convenience
- `replaceColor`, `recolor`, `duotone`, `gradientMap`, `gradientRecolor`, `gradientFill`
- `applyLut`, `curves`, `levels`, `whiteBalance`, `colourBalance`/`colorBalance`
- `posterize`, `solarize`, `fill`, `paletteSwap`, `alphaFromLuma`
- Gradient builder with linear/radial types, arbitrary stops, and spread modes

## Explicitly excluded formats

TIFF, JPEG 2000/JP2, HEIC/HEIF, PDF. These are rejected with `UnsupportedFormatError` or a JavaScript error. AVIF is supported because it is a modern AV1-based format.

## Untrusted input

- **AVIF decode** uses `rav1d`, which reports some corrupt streams from inside `extern "C"` entry points; a Rust panic there cannot unwind and would abort the process. The workspace therefore builds a patched copy of `rav1d 1.1.0` (`vendor/rav1d`, wired in through `[patch.crates-io]`; the one-line fix is marked `fastpkg patch` in `src/decode.rs`) whose frame-decode error path returns an error instead of unwrapping an already released frame header. Corrupt AVIFs now reject with an `Error`. Residual risk: any other panic inside rav1d's C-ABI functions would still abort; mutation fuzzing of an AVIF (400 mutated files, plus the robustness suite) found none after the fix. rav1d also logs `Error parsing OBU data` to stderr for such files. Drop the vendored copy once an upstream release contains the fix.
- **SVG** input is limited to 257 levels of element nesting (libxml2/librsvg refuses deeper documents too), is parsed and rendered on a worker thread with a 64 MiB stack, and rejects `feMorphology radius > 256`, `feConvolveMatrix order > 32` and `feTurbulence numOctaves > 32` up front because those cost minutes of CPU per render. Other CPU-heavy but legal constructs (for example a very large `feGaussianBlur` on a multi-megapixel canvas, which takes tens of seconds) are bounded only by `limitInputPixels` and the `timeout()` checks between pipeline stages.
