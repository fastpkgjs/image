# @fastpkg/image

A Rust image engine for Node.js with a sharp-compatible API. No libvips, no ImageMagick, no legacy TIFF/JPEG 2000/HEIC/PDF pipeline. JPEG uses the bundled MozJPEG/libjpeg-turbo codec for speed; the pipeline, PNG, WebP, AVIF, JPEG XL, QOI, GIF, SVG, and operations are Rust.

`@fastpkg/image` is built around three ideas:

1. **Do not copy pixels unless the operation requires it.** File inputs are memory-mapped, Buffer inputs are pinned across async work, and encoded output is handed to Node as an external Buffer.
2. **Get the answer from the header when possible.** `probe()` and `metadata()` parse only the bytes needed for dimensions, orientation, alpha, and metadata.
3. **Cross the JavaScript boundary once per pipeline.** The JS builder accumulates a pipeline spec and calls native code at the terminal operation (`toBuffer`, `toFile`, `metadata`, ...).

## Status

This is a working v0.1 implementation with benchmark wins across the current scenarios, but it is not a finished sharp replacement. The current benchmark is on Windows x64 with an i5-12450H. Cross-platform build targets are configured, but prebuilt binaries for macOS/Linux still need to be produced by CI.

## Requirements

- Node.js 20.17 or newer (tested on Node 26.4)
- Rust 1.99.1 or newer
- A C/C++ compiler for MozJPEG (Visual Studio Build Tools on Windows, clang/gcc elsewhere)
- NASM for MozJPEG SIMD and the AV1 decoder used by AVIF (NASM is auto-downloaded to `tools/nasm-2.16.03` by the Windows build script when missing)
- CMake is not required by the default build

## Build

```bash
npm install
npm run build:native
npm test
npm run bench
```


`npm run build:native` invokes `scripts/build.mjs`. On Windows it delegates to `scripts/build.ps1`, which imports the MSVC environment, adds the auto-downloaded NASM to `PATH`, and builds the N-API addon into `packages/image`. On macOS/Linux it calls `napi build` directly.

The addon is loaded by `packages/image/lib.cjs`. The generated JS binding is disabled on purpose (`--no-js`); the hand-written wrapper is the public API.

The external 16-bit PNG fixtures can be regenerated with `python scripts/make_png16_fixtures.py`.

## Releases

Push a tag such as `v0.1.0` to run `.github/workflows/release.yml`. The workflow builds native addons for Windows x64, macOS x64/arm64, and Linux x64/arm64 glibc, publishes them as `@fastpkg/image-*` platform packages, publishes `@fastpkg/image`, and creates a GitHub release with the `.node` binaries attached.

The publish job requires an `NPM_TOKEN` repository secret with access to the `@fastpkg` scope. `@fastpkg/image` declares the platform packages as `optionalDependencies`, and the loader resolves the matching one at runtime.

## Usage

```js
import image from '@fastpkg/image';

const info = await image('input.jpg')
  .rotate()
  .resize({ width: 1200, fit: 'cover' })
  .webp({ lossless: true })
  .toFile('output.webp');

console.log(info.width, info.height, info.size);
```

Sharp-style callbacks are supported:

```js
await image('input.png')
  .resize(320)
  .jpeg({ quality: 82 })
  .toBuffer((error, data, info) => {
    if (error) throw error;
    console.log(info);
  });
```

Streams work as a Duplex:

```js
import fs from 'node:fs';
import image from '@fastpkg/image';

fs.createReadStream('input.jpg')
  .pipe(image().resize(800).webp({ lossless: true }))
  .pipe(fs.createWriteStream('output.webp'));
```

Header-only probing:

```js
const header = await image('large.jpg').probe();
// { format: 'jpeg', width, height, channels, hasAlpha, orientation, ... }
```

Tiles and zip containers:

```js
await image('input.png')
  .jpeg({ quality: 80 })
  .tile({ layout: 'dz', size: 256 })
  .toFile('output.dz');
// writes output.dzi and output_files/{level}/{col}_{row}.jpg

await image('input.png')
  .jpeg({ quality: 80 })
  .tile({ layout: 'google', size: 256 })
  .toFile('tiles.zip');
// layouts: dz, google, zoomify, iiif, iiif3; .zip/.szi writes a zip archive
```

## Formats

| Format | Decode | Encode | Notes |
|---|---|---|---|
| PNG | Yes | Yes | 8/16-bit, alpha, palette input/output, interlaced input |
| JPEG | Yes | Yes | MozJPEG codec, baseline/progressive, EXIF orientation, DCT-scaled decode |
| WebP | Yes | Yes | Lossless via image-webp, lossy via oxideav-webp |
| AVIF | Yes | Yes | Pure-Rust decode via rav1d/avif-decode; encode via ravif |
| JPEG XL | Yes | Optional | Pure-Rust decode via jxl-oxide; encode behind `jxl-encode` (CMake, ClangCL, libjxl) |
| QOI | Yes | Yes | |
| GIF | Yes (first frame) | Yes (single frame) | Animation is not implemented yet |
| SVG | Yes | No | resvg/usvg; no network or script execution |
| Raw | Yes | Yes | Explicit width/height/channels; 8-bit or 16-bit `ushort` |
| TIFF, JP2, HEIC, PDF | No | No | Explicit unsupported errors |

16-bit PNG and raw `ushort` samples are preserved for decode/encode, raw round-trips, and geometry operations (`resize`, `extract`, `extend`, `rotate`, `affine`, `flip`, `flop`, `autoOrient`). Point and filter operations currently run on the 8-bit representation and discard the wide buffer. Use `.toColourspace('rgb16')` or `.png({ bitdepth: 16 })` for 16-bit output.

## API surface

Implemented sharp-compatible methods:

- Input: `resize`, `extend`, `extract`, `trim`, `rotate`, `autoOrient`, `flip`, `flop`, `affine`, `sharpen`, `median`, `blur`, `dilate`, `erode`, `flatten`, `unflatten`, `gamma`, `negate`, `normalise`/`normalize`, `clahe`, `convolve`, `threshold`, `boolean`, `linear`, `recomb`, `modulate`, `tint`, `greyscale`/`grayscale`, `removeAlpha`, `ensureAlpha`, `extractChannel`, `joinChannel`, `bandbool`, `composite`
- Output: `toFile`, `toBuffer`, `toUint8Array`, `toFormat`, `jpeg`, `png`, `webp`, `gif`, `avif`, `jxl`, `qoi`, `raw`, `tile`, `timeout`
- Metadata output: `withDensity`, `keepExif`, `withExif`, `withExifMerge`, `keepIccProfile`, `withIccProfile`, `keepXmp`, `withXmp`, `keepGainMap`, `withGainMap`, `keepMetadata`, `withMetadata`
- Input metadata: `metadata`, `stats`, `probe`
- Utilities: `cache`, `concurrency`, `counters`, `simd`, `format`, `versions`, `interpolators`, `queue`, `block`, `unblock`
- Extras: `toStream`, `responsive`, `dominant`, `joinChannel`
- Colour extensions: `replaceColor`, `recolor`, `duotone`, `gradientMap`, `gradientRecolor`, `gradientFill`, `applyLut`, `curves`, `levels`, `whiteBalance`, `colourBalance`, `posterize`, `solarize`, `fill`, `paletteSwap`, `alphaFromLuma`

Known gaps are documented in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md). Remaining major gaps: animation, JXL output without the optional native feature, CMYK/Lab colour spaces, 16-bit point/filter operations, gain maps, raw depths above 16-bit, and true incremental decode for every codec.

## Colour, gradients, and LUTs

Fastpkg adds native colour tooling beyond sharp:

```js
const gradient = image
  .gradient()
  .linear(45)
  .addStop(0, '#0f172a')
  .addStop(0.5, '#7c3aed')
  .addStop(1, '#f8fafc');

await image('in.png')
  .replaceColor('#ff00ff', '#00ffff', { tolerance: 0.12 })
  .recolor('#2563eb', { amount: 0.8, preserveOpacity: true })
  .duotone('#0b1020', '#f6d365')
  .curves({ rgb: [[0, 0], [0.5, 0.78], [1, 1]] })
  .levels({ inputBlack: 12, inputWhite: 242, gamma: 1.05 })
  .whiteBalance({ temperature: 18, tint: -4 })
  .colourBalance({ shadows: [-8, 0, 12], highlights: [10, 0, -10] })
  .posterize(6)
  .solarize(140)
  .gradientFill(gradient, { preserveOpacity: true })
  .applyLut('film.cube', { strength: 0.85 })
  .toFile('out.png');
```

Also included: `gradientMap`, `gradientRecolor`, `fill`, `paletteSwap`, `alphaFromLuma`, and radial/multi-stop gradients via `image.gradient()`. See [docs/COLOR.md](docs/COLOR.md) for semantics, options, and LUT format.

## Zero-copy and threading

- File input is mapped with `memmap2`; no input copy.
- Async Buffer input is pinned by napi-rs `Buffer` for the duration of the task. The caller must not mutate it while the pipeline runs.
- Encoded output is `Buffer::from(Vec<u8>)`, which transfers ownership to Node without a copy.
- The JS API produces one `PipelineSpec`; Rust executes the whole plan on a dedicated worker thread.
- Resize uses `fast_image_resize` SIMD kernels (AVX2/SSE4.1/NEON) and rayon tiling. AVIF/JXL/PNG/JPEG codecs use their own internal threading where available.
- JPEG targets are DCT-scaled during decode when a resize hint is present.

## Benchmarks

Run:

```bash
node bench/run.mjs
```

Current results (median ms, `sharp@0.35.5`, concurrency 1, Windows x64, i5-12450H, Node 26.4):

| Scenario | Fastpkg | sharp | Ratio |
|---|---:|---:|---:|
| metadata 800x600 | 0.09 | 0.93 | 10.63x faster |
| metadata 4000x3000 | 0.14 | 1.23 | 8.62x faster |
| resize 1200 + JPEG q85 | 17.95 | 26.43 | 1.47x faster |
| resize 1200 + PNG | 18.17 | 28.97 | 1.59x faster |
| resize 1200 + WebP lossless | 21.07 | 578.11 | 27.44x faster |
| blur + sharpen + JPEG | 50.81 | 79.32 | 1.56x faster |
| stats 4000x3000 | 105.89 | 475.03 | 4.49x faster |
| 8 concurrent resizes | 8.40 | 44.83 | 5.34x faster |

These are end-to-end Node timings, not codec microbenchmarks. Fastpkg now uses MozJPEG for JPEG decode/encode and DCT-scaled decode, while keeping libvips out of the pipeline. The remaining caveat is that this is a single laptop benchmark run, not a controlled release benchmark.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design, [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for the API matrix, and [docs/COLOR.md](docs/COLOR.md) for colour/gradient/LUT semantics.

## Security

- Header probes are checked against dimension and memory limits.
- SVG resources are local; no scripts or network access.
- Decoders are exposed to untrusted input and should be fuzzed before production use.
- No Rust panic is allowed to cross the N-API boundary in the intended design; worker panics become JavaScript errors.

## License

MIT.
