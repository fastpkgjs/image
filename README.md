# @fastpkg/image

A Rust image engine for Node.js with a sharp-compatible API. No libvips, no ImageMagick, no legacy TIFF/JPEG 2000/HEIC/PDF pipeline. JPEG uses the bundled MozJPEG/libjpeg-turbo codec and WebP decoding and lossy WebP encoding use the bundled libwebp, both for speed; the pipeline, PNG, the fast lossless WebP path, AVIF, JPEG XL, QOI, GIF, SVG, and operations are Rust.

`@fastpkg/image` is built around three ideas:

1. **Do not copy pixels unless the operation requires it.** File inputs are memory-mapped, Buffer inputs are pinned across async work, and encoded output is handed to Node as an external Buffer.
2. **Get the answer from the header when possible.** `probe()` and `metadata()` parse only the bytes needed for dimensions, orientation, alpha, and metadata.
3. **Cross the JavaScript boundary once per pipeline.** The JS builder accumulates a pipeline spec and calls native code at the terminal operation (`toBuffer`, `toFile`, `metadata`, ...).

## Status

Version 0.2.0. The API follows sharp closely and covers the common resize/convert/composite/colour workflows (see [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for what is and is not implemented), but it is not a complete sharp replacement. Prebuilt binaries are published to npm for the platforms below; see [CHANGELOG.md](CHANGELOG.md) for what changed in each release.

The benchmark numbers further down were measured on Linux x64 (shared 12 vCPU VM) for 0.2.0. Other platforms have not been benchmarked.

### Supported platforms

`npm install @fastpkg/image` installs exactly one prebuilt binary through `optionalDependencies`. No compiler is needed.

| Platform | Architecture | Package | libc |
| --- | --- | --- | --- |
| Windows | x64 | `@fastpkg/image-win32-x64` | MSVC |
| Windows | arm64 | `@fastpkg/image-win32-arm64` | MSVC |
| macOS | x64 | `@fastpkg/image-darwin-x64` | |
| macOS | arm64 (Apple silicon) | `@fastpkg/image-darwin-arm64` | |
| Linux | x64 | `@fastpkg/image-linux-x64-gnu` | glibc (built on Ubuntu 22.04, glibc 2.35) |
| Linux | arm64 | `@fastpkg/image-linux-arm64-gnu` | glibc (built on Ubuntu 22.04, glibc 2.35) |
| Linux | x64 | `@fastpkg/image-linux-x64-musl` | musl (Alpine) |
| Linux | arm64 | `@fastpkg/image-linux-arm64-musl` | musl (Alpine) |

The loader detects musl at runtime and picks the `-musl` package on Alpine. Other platforms (FreeBSD, 32-bit, riscv, ...) are not supported. If the binary cannot be loaded, the error names the platform, libc, and the package it looked for. The usual fix is to reinstall with optional dependencies enabled (`npm install --include=optional`) and to avoid copying `node_modules` between operating systems or between glibc and musl.

On Windows arm64 the bundled MozJPEG is built without its SIMD routines (they require the GNU assembler), so JPEG encode/decode is slower there than on other platforms.

## Requirements

Using the package:

- Node.js 22 or newer (the active LTS lines 22 and 24, and Node 26). CI runs the test suite on Node 22, 24, and 26.

Building from source (only needed for development or unsupported platforms):

- Rust 1.99.1 (pinned in `rust-toolchain.toml`)
- A C compiler for MozJPEG and libwebp (Visual Studio Build Tools on Windows, clang/gcc elsewhere; `build-base` on Alpine)
- NASM for MozJPEG SIMD and the AV1 decoder used by AVIF on x64 (NASM is auto-downloaded to `tools/nasm-2.16.03` by the Windows build script when missing)
- CMake is not required by the default build

## Build

```bash
npm install
npm run build:native
npm test
npm run bench
```


`npm run build:native` invokes `scripts/build.mjs`. On Windows it delegates to `scripts/build.ps1`, which imports the MSVC environment, adds the auto-downloaded NASM to `PATH`, and builds the N-API addon into `packages/image`. On macOS/Linux it calls `napi build` directly. On Alpine, install `build-base nasm` (plus `linux-headers` and `perl`) and Rust via rustup; `scripts/ci-musl.sh build` shows the exact steps CI uses.

The addon is loaded by `packages/image/lib.cjs`. The generated JS binding is disabled on purpose (`--no-js`); the hand-written wrapper is the public API.

The external 16-bit PNG fixtures can be regenerated with `python scripts/make_png16_fixtures.py`.

## Releases

Push a tag such as `v0.2.0` to run `.github/workflows/release.yml`. The tag must equal the version in `package.json`, the Cargo workspace (`Cargo.toml`), and `Cargo.lock`, otherwise the workflow fails before building.

1. The workflow reuses `.github/workflows/ci.yml` to build the native addon for all eight targets and to run `npm test` and `tests/smoke.mjs` against each built binary on its own OS (Linux glibc x64/arm64, macOS x64/arm64, Windows x64/arm64, and Alpine x64/arm64 in `node:*-alpine` containers) on Node 22, 24, and 26.
2. Only if all of that passes, the publish job stages the packages with `scripts/release-packages.mjs` (which also checks manifests, versions, `optionalDependencies`, and the `npm pack` file list), publishes each `@fastpkg/image-*` platform package, then publishes `@fastpkg/image` last, and creates a GitHub release with the `.node` binaries attached.
3. Publishing skips any package whose exact version already exists on npm, so a failed run can be re-run.

The publish job requires an `NPM_TOKEN` repository secret with access to the `@fastpkg` scope. `@fastpkg/image` declares the platform packages as `optionalDependencies`, and the loader resolves the matching one at runtime.

To check the packaging locally without publishing: `node scripts/check-version.mjs v0.2.0`, then `node scripts/release-packages.mjs --version 0.2.0 --artifacts <dir with the eight .node files> --output <dir outside the repo>`.

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
| WebP | Yes | Yes | Lossy and near-lossless via libwebp (BSD-3, bundled via libwebp-sys); lossless via image-webp (fast), switching to libwebp for graphic-like images or when `effort` is set; decoding via libwebp (animated files fall back to image-webp, first frame only) |
| AVIF | Yes | Yes | Pure-Rust decode via rav1d/avif-decode; encode via ravif |
| JPEG XL | Yes | Optional | Pure-Rust decode via jxl-oxide; encode behind `jxl-encode` (CMake, ClangCL, libjxl) |
| QOI | Yes | Yes | |
| GIF | Yes (first frame) | Yes (single frame) | Animation is not implemented yet |
| SVG | Yes | No | resvg/usvg; no network or script execution |
| Raw | Yes | Yes | Explicit width/height/channels; 8-bit or 16-bit `ushort` |
| TIFF, JP2, HEIC, PDF | No | No | Explicit unsupported errors |

16-bit PNG and raw `ushort` samples are preserved for decode/encode, raw round-trips, and geometry operations (`resize`, `extract`, `extend`, `rotate`, `affine`, `flip`, `flop`, `autoOrient`). Point and filter operations currently run on the 8-bit representation and discard the wide buffer. Use `.toColourspace('rgb16')` or `.png({ bitdepth: 16 })` for 16-bit output.

### PNG output

`png()` takes sharp's options and maps them like this:

| Option | Behaviour |
|---|---|
| `compressionLevel` (0-9, default 6) | A zlib ladder as in libpng/sharp: `0` stored (uncompressed) blocks, `1`-`3` fast, `4`-`6` default, `7`-`9` strongest. Deflate is zlib-rs; large images are deflated as fixed 512 KiB blocks in parallel and joined into one zlib stream (about 0.04% larger than one stream). |
| `adaptiveFiltering` | `true` filters every row adaptively, `false` writes unfiltered rows (what libvips does by default). Left unset, the encoder trial-compresses a sample of rows both ways and keeps the smaller (adaptive only when it wins by 2% or more), so the default output is never meaningfully larger than sharp's and is often several times smaller on photos and gradients. |
| `palette`, `quality`, `effort`, `colours`/`colors`, `dither` | Indexed output through a built-in median-cut quantiser with error diffusion (no libimagequant, which is GPL). Like sharp, giving `quality`, `effort`, `colours` or `dither` switches `palette` on. Images with at most `colours` distinct colours (alpha included) are converted losslessly. `colours` picks a 1/2/4/8-bit depth (2/4/16/256 entries) exactly as libvips does; translucent entries are written to `tRNS`. |
| `progressive` | Adam7 interlacing (also for palette output). |

The colour type and channel count are never changed behind your back: RGBA stays RGBA unless `palette` is requested.

PNG bytes are **not** libvips-identical, but the encoder is a pure function of the pixels and the options: the same input gives the same bytes on every run, with any thread count (`image.concurrency()`), and, by construction, on every platform. They are stable within a release line (pinned by `tests/png-stability.test.mjs`); any change to the encoded bytes is listed in the CHANGELOG.

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

## Parallel JPEG encoding (opt-in)

```js
await image(input).jpeg({ quality: 85, parallel: true }).toBuffer();
```

`parallel: true` splits images of 1 megapixel or more into horizontal bands (one per rayon thread, at least 64 rows each), encodes the bands concurrently as baseline JPEGs, and joins them into one standard file with restart markers (`DRI`/`RSTn`) at the band boundaries. Every band uses the same quality, subsampling and the standard Huffman tables, and the DC predictors and bit padding at a band edge are exactly what a restart marker resets, so the decoded pixels are identical to a single-pass baseline encode with the same settings (`optimiseCoding: false`). EXIF, ICC and density options are applied to the joined file as usual.

Trade-offs:

- Off by default; the default encoder and its output bytes are unchanged.
- Because the Huffman tables are the standard ones, `optimiseCoding` is ignored and files are larger than the default optimised-table output. On a 4000x3000 photo that was +7.5% at q85, +16% at q60 and +17% at q95; very smooth or flat images can grow much more (a synthetic 4000x3000 test image was +48% to +140%). Compared with `optimiseCoding: false` the cost is a few dozen bytes of markers.
- It cannot be combined with `progressive`, `mozjpeg` or `optimiseScans`; that combination throws a `TypeError`.
- Images under 1 megapixel, or a thread pool of one (`image.concurrency(1)`), silently use the normal encoder.
- Only the encode stage is parallel. Decoding the source and resizing are unchanged, so the end-to-end gain depends on how much of the pipeline is JPEG encoding: a 4000x3000 raw -> JPEG q85 went from 78 ms to 20 ms on a 12-thread machine, a JPEG -> JPEG re-encode of the same size from about 105 ms to about 46 ms (decoding is the remaining floor).

## Benchmarks

Run:

```bash
node bench/run.mjs
```

Current results (the PNG rows were re-measured separately after the PNG encoder rewrite; median ms, `sharp@0.35.5`, concurrency 1, Linux x64, AMD EPYC-Rome 12 vCPU, Node 24):

| Scenario | Fastpkg | sharp | Ratio |
|---|---:|---:|---:|
| metadata 800x600 | 0.10 | 0.56 | 5.4x faster |
| metadata 4000x3000 | 0.14 | 0.52 | 3.6x faster |
| resize 1200 + JPEG q85 (synthetic gradient) | 29.8 | 36.1 | 1.21x faster |
| photo JPEG 4000x3000 -> resize 1200 JPEG | 41-48 | 45 | about equal |
| photo JPEG -> thumbnail 320 JPEG | 16.7 | 21.1 | 1.26x faster |
| photo JPEG -> JPEG q85, no resize | 113-120 | 98-101 | 12-18% slower (single thread) |
| photo JPEG -> resize 1200 WebP q80 | 158 | 171 | 1.08x faster |
| resize 1200 + PNG (default level) | 97.0 | 49.3 | 2.0x slower single-thread, 40% smaller output; 1.3x faster with the default pool |
| photo PNG 2000 -> PNG (default level) | 531 | 287 | 1.85x slower single-thread, 3.0x smaller output; 2.2x faster with the default pool |
| alpha PNG 2000 -> resize 800 PNG | 94.0 | 113 | 1.2x faster, 1.9x smaller output |
| graphic PNG 1200 -> palette PNG | 56 | 113 | 2.0x faster |
| photo PNG 2000 -> palette PNG | 1325 | 11050 | 8.3x faster |
| resize 1200 + WebP lossless | 34.7 | 676.2 | 19.5x faster |
| blur + sharpen + JPEG | 26.4 | 58.9 | 2.2x faster |
| stats 4000x3000 | 141.2 | 419.3 | 3.0x faster |
| 8 concurrent resizes | 4.7 | 25.7 | 5.5x faster |

With the default thread pool (the table pins both engines to one thread), large raw/PNG conversions run in parallel, and `jpeg({ parallel: true })` (opt-in, see below) encodes a 4000x3000 image in about 14 ms instead of about 75 ms at the cost of 5-17% larger files.

These are end-to-end Node timings, not codec microbenchmarks. Fastpkg now uses MozJPEG for JPEG decode/encode and DCT-scaled decode, while keeping libvips out of the pipeline. The remaining caveat is that this is a single laptop benchmark run, not a controlled release benchmark.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design, [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) for the API matrix, and [docs/COLOR.md](docs/COLOR.md) for colour/gradient/LUT semantics.

## Security

- Header probes are checked against dimension and memory limits.
- SVG resources are local; no scripts or network access.
- Decoders are exposed to untrusted input and should be fuzzed before production use.
- No Rust panic is allowed to cross the N-API boundary in the intended design; worker panics become JavaScript errors.

## License

MIT.
