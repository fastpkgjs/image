# Changelog

## 0.2.0

### Fixes

- **AVIF decode no longer aborts the process on corrupt files.** `rav1d` unwrapped an already released frame header in its error path inside an `extern "C"` function; the build now uses a patched vendored copy (`vendor/rav1d`) so such files reject with an `Error`.
- **SVG:** documents nested deeper than 257 elements are rejected (they overflowed the stack and killed the process), parse/render runs on a 64 MiB worker stack, and filter parameters that cost minutes of CPU (`feMorphology`, `feConvolveMatrix`, `feTurbulence`) are rejected.
- `fit: 'contain'` now pads to the requested size with `background`/`position` (alpha is added for translucent backgrounds), also with a single dimension and with `withoutReduction`.
- `withoutEnlargement`/`withoutReduction` follow sharp's per-axis semantics, `fit: 'fill'` with one dimension keeps the other axis, tiny `outside`/`cover` targets round like sharp, and `cover` scales then crops like libvips.
- Numeric `position`/gravity values and `sharp.gravity`/`sharp.strategy` are accepted and exported (`entropy`/`attention` fall back to a centre crop). Composite `gravity` accepts numbers too.
- EXIF orientations 5 (transpose) and 7 (transverse) are applied correctly by `rotate()`/`autoOrient()`; the retained EXIF Orientation is reset to 1 afterwards, and `withMetadata({ orientation, density })` overrides the retained values (JPEG, PNG, WebP, JXL).
- JPEG output from RGBA input composites onto black instead of keeping the colour under transparent pixels.
- `rotate(angle)` and `affine()` size and position their output like libvips (rounded bounding box; `idx`/`idy`/`odx`/`ody` move the content, not the frame).
- Operations are executed in sharp's fixed pipeline order instead of call order (see `docs/COMPATIBILITY.md`).
- `extend()` with a translucent background adds an alpha channel.
- 16-bit PNG upscaling was reported as producing wrong colours; investigation showed the 16-bit path is equivalent to the 8-bit path (differences came from a noise-like test fixture), so no engine change was needed.
- **Wrong resize size on large JPEGs.** `resize({ width, height, fit: 'inside' })` (and width-only/`cover`/`outside`) on a JPEG could return the wrong size when the source was large enough for DCT shrink-on-load (3000x1500 gave 1124x562 instead of 1200x600, 4000x2000 gave 1000x500). Output size is now always derived from the full-resolution source, never from the already-shrunk decode. Shrink-on-load is only used when the resize is the first operation, accounts for EXIF auto-orientation, and can no longer pick a scale that undershoots the target on either axis.
- `fit: 'inside'` now enlarges smaller images like sharp does, unless `withoutEnlargement` is set (it was silently treated as always `withoutEnlargement`).
- **Wrong channel count after `extractChannel()`.** Raw output of `extractChannel()` and `toColourspace('b-w')` is now a single channel like sharp, and later operations (resize, blur, extend, ...) keep it. If a later operation introduces colour the output falls back to RGB instead of losing it. `toColourspace('b-w')` keeps alpha (2 channels, like sharp) instead of dropping it. `greyscale()` keeps three identical channels in PNG/JPEG/WebP output, and plain `raw()` after `greyscale()` is single-channel, matching sharp.
- Grey inputs (8/16-bit grey PNG, grey JPEG, 1/2-channel raw input) are expanded to RGB(A) like sharp, so channel counts match.
- `extend()` now supports sharp's `{ top, left, bottom, right }` and single-number forms (previously only `width`/`height` were honoured and the call was a silent no-op otherwise). Per-edge values are validated.
- Colour options accept sharp's `{ r, g, b, alpha }` objects everywhere a background/tint colour is taken (resize, extend, rotate, affine, trim, flatten, tint, `create`, tile).
- EXIF/ICC/XMP metadata now survives every operation with `keepExif()`/`keepIccProfile()`/`keepXmp()`; `blur`, `sharpen`, `extract` and others dropped it.
- `rotate(angle)` with a non-right angle and `affine()` no longer add an alpha channel to opaque images unless the background is transparent (RGB input gave 4-channel output).

- **sharp-style argument validation.** Every public option is now checked synchronously with sharp's wording (`Expected integer between 1 and 100 for quality but received 0 of type number`): `resize`, `extend`, `extract`, `trim`, `blur`, `sharpen`, `gamma`, `clahe`, `threshold`, `modulate`, `tint`, `composite`, all output formats, `tile`, `timeout`, colour strings/objects (CSS names, hex, `rgb()`, `hsl()`, ...) and every constructor option. Errors are `TypeError`/`Error`, `null` behaves like `undefined`, and native errors no longer carry the `InvalidArg, ` / `GenericFailure, ` status prefix. `sharp.gravity`, `fit`, `kernel`, `blend`, ... constants are exported and numeric gravity/position values work.
- **libvips-exact filters.** `greyscale`/`toColourspace('b-w')` (sRGB B_W luma), `threshold` (alpha band kept), `gamma` (two-stage 8-bit quantisation), `linear` (alpha untouched), `convolve` (premultiplied RGBA), `modulate` and `tint` (CIELAB/LCh), `normalise` (Lab lightness percentiles), `clahe` (libvips `hist_local`), `trim` (`find_trim`: max-band threshold, uniform images unchanged) and `boolean` with differently sized operands now match sharp to within one grey level on the test corpus (previously up to 130 levels apart).
- **composite.** All sharp blend modes follow libvips' arithmetic; Porter-Duff operators act on the whole canvas; a second `composite()` call replaces the first; `tile`, `premultiplied`, raw buffers and `create`/`text` overlays work; numeric gravity is accepted.
- **Output.** `webp({ nearLossless })` writes near-lossless VP8L, GIF keeps 1-bit transparency, `png({ progressive })` writes Adam7 PNGs, `jpeg({ mozjpeg })` implies progressive output and RGBA input is flattened onto black, `extend()` with a translucent background adds alpha, `withIccProfile('srgb' | 'p3')` embeds a generated ICC v4 profile, `heif({ compression: 'av1' })` encodes AVIF.
- **Metadata.** `depth` is `uchar`/`ushort`, `space` reports `rgb16`/`grey16`, `density` (JFIF/EXIF/pHYs), `chromaSubsampling`, `isProgressive` for interlaced PNG, alpha/channels for WebP/GIF/AVIF, `pages`/`loop`/`delay` for animated GIF/WebP and PNG `iCCP` profiles are reported from the headers; unknown fields are omitted instead of `null`.
- **Input.** `raw` input accepts 2 channels (grey + alpha), `failOn` follows sharp (truncated/corrupt JPEG and WebP with a bad RIFF size are rejected, `failOn: 'none'` returns the decodable part of PNG/GIF), SVG `density` scales the raster, `stats()` of 16-bit images uses the 0..65535 range and `create: { noise }` is supported.

### Performance

- `jpeg({ parallel: true })` (opt-in): encodes images of 1 MP or more as concurrent bands joined with restart markers. Decoded pixels equal a single-pass baseline encode. It ignores `optimiseCoding`, uses the standard Huffman tables (files are about 5-17% larger on photos, more on very smooth images), throws a `TypeError` together with `progressive`/`mozjpeg`/`optimiseScans`, and falls back to the normal encoder for small images or a single thread. A 4000x3000 raw -> JPEG q85 encode drops from about 75 ms to about 14 ms on 12 threads.
- Raw/packed/RGB conversions, alpha flatten for JPEG output, unpremultiply, raw-input expansion and the greyscale op are parallel for large images (JPEG -> raw at 4000x3000 about 15 ms faster). Output bytes are unchanged.
- Arbitrary-angle `rotate`/`affine`, `flip`/`flop`, quarter-turn rotation, `median`, `extend` (copy/mirror/repeat) and `blur` are row-parallel or restructured (rotate 33° 2.5x faster, affine 3x, median 5x5 3x); `blur`+`sharpen` pipelines ~25% faster; the SSE4.1 `unsafe` blur kernel was replaced by safe code with bit-identical output.
- JPEG decode always uses libjpeg (MozJPEG) now, including pipelines without a resize hint (~40 ms faster on a 12 MP image, pixel-identical to sharp's decode).
- PNG decode/encode: fewer passes and copies; compression levels 7-9 use zlib-rs (2.6x faster at level 7, same size within 1%).
- `extractChannel`, `trim` and other per-pixel ops no longer clone the image buffer.
- WebP: lossy and near-lossless encoding now use libwebp (BSD-3, bundled via `libwebp-sys`; needs only a C compiler), about 4x faster than the previous pure-Rust encoder (photo JPEG -> resize 1200 -> WebP q80: 645 ms to 155 ms, now ahead of sharp) with file sizes within a few percent of sharp's. `alphaQuality` is now honoured (it was silently ignored). Lossless WebP still uses the pure-Rust encoder. Added `THIRD_PARTY_NOTICES.md`.

- WebP lossless: libwebp is used whenever `effort` is given (`effort` maps to method, `quality` to its size/speed trade-off). By default the fast pure-Rust coder is still used, but graphic-like images (flat colours, text, screenshots, icons; encoded output under 5% of raw size, 1-16 MP) are also tried with libwebp method 1 and the smaller result is kept, shrinking such files 3x to 8x (e.g. 126 KB to 14.7 KB), to within ~5% of sharp's size. Photographs remain 1.2-1.6x larger than sharp's lossless output in exchange for ~17x speed.
- WebP: added the `exact` option (preserve colours under transparent pixels). Lossless WebP now keeps ICC profiles and XMP set via `withMetadata`/`withXmp`, and images wider or taller than 16383 px are rejected instead of producing an invalid file.

### Platforms and packaging

- **Node.js 22 or newer is now required** (`engines`), matching the supported lines: CI tests Node 22, 24 and 26.
- New prebuilt binaries: Linux musl (Alpine) x64 and arm64 (`@fastpkg/image-linux-x64-musl`, `@fastpkg/image-linux-arm64-musl`) and Windows arm64 (`@fastpkg/image-win32-arm64`). Windows arm64 builds MozJPEG without SIMD.
- The loader detects musl and loads the matching `-gnu` or `-musl` package. Previously every Linux system was mapped to the glibc package, so Alpine failed with an unhelpful error.
- The "native addon not found" error now reports platform, architecture, libc, the package it expected, the underlying load errors, and how to reinstall optional dependencies.
- Linux glibc binaries are now built on Ubuntu 22.04 (glibc 2.35) instead of the newest runner image; arm64 glibc is built natively instead of with `--use-napi-cross`.
- `npm test` now goes through `scripts/test.mjs`, which lists test files itself so it also works on Node 20 and on Windows shells that do not expand globs.

### CI and release

- CI builds every target once and runs `npm test` and `tests/smoke.mjs` on Node 22, 24, and 26 on Linux x64 (Ubuntu 22.04 and latest), Linux arm64, macOS arm64, macOS x64, Windows x64, Windows arm64, and Alpine x64/arm64. Lint (`cargo fmt`, `clippy`) runs as a separate job.
- The release workflow reuses the CI jobs, so a binary is tested on its target OS before anything is published.
- The release fails early if the tag does not match `package.json`, the Cargo workspace version, and `Cargo.lock` (`scripts/check-version.mjs`).
- Publishing skips package versions that already exist on npm, and the main `@fastpkg/image` package is published last, after all platform packages are confirmed on the registry.
- `scripts/release-packages.mjs` now self-checks the staged output (names against the loader, versions, `optionalDependencies`, `npm pack` contents).

## 0.1.0

- Initial release: sharp-compatible Rust image engine for Node.js with prebuilt binaries for Windows x64, macOS x64/arm64, and Linux glibc x64/arm64.
