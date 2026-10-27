# Changelog

## Unreleased

### Memory and speed

Peak RSS of one fresh process (`bench/peak-rss.mjs`; fixtures from `bench/gen-huge-png.mjs`, so they are reproducible), the 0.5.0 build against this one, with sharp 0.35.5 for scale. `rotate().resize(1200).jpeg()` unless noted:

| input | 0.5.0 | now | sharp |
|---|---|---|---|
| 16-bit RGB 40 MP (6325x6325), gradient | 366 MB | 165 MB | 151 MB |
| 16-bit RGB 40 MP, noise (229 MB file) | 592 MB | 397 MB | 380 MB |
| 16-bit RGB 31 MP (5568x5568), gradient | 949 MB | 165 MB | 142 MB |
| 16-bit RGBA 31 MP, gradient | 1186 MB | 234 MB | 167 MB |
| 16-bit RGB 31 MP, noise (177 MB file) | 1126 MB | 342 MB | 318 MB |
| 16-bit RGB 40 MP to 3000 px (mild downscale), gradient | 1341 MB | 468 MB | 175 MB |
| 16-bit RGB 40 MP to 3000 px, noise | 1569 MB | 694 MB | 405 MB |
| 8-bit RGB 31 MP, gradient / noise | 208 / 294 MB | 207 / 298 MB (unchanged: 124 MB decoded, under the cutoff) | 111 / 200 MB |
| Adam7 8-bit RGB 256 MP to 800 px | 1134 MB, 2.8 s | 133 MB, 1.4 s | 904 MB, 6.6 s |
| Adam7 8-bit RGBA 256 MP to 800 px | 2160 MB, 3.6 s | 136 MB, 1.9 s | 1196 MB, 8.2 s |
| Adam7 16-bit RGB 256 MP to 800 px | over 6 GB (killed) | 152 MB, 2.7 s | |
| non-interlaced 8-bit RGB 256 MP to 1200 px | 104 MB, 1.2 s | 173 MB, 1.8 s | |

The spread in the 16-bit reports was the shrink factor, not the content: with a large downscale the decode-time shrink applies and the cost is the intermediate; with a mild one (the target is more than half the source) nothing can shrink, the whole 16-bit frame is resident and every copy of it counts. Gradient versus noise only changes the input file size; RGB versus RGBA and grey change the bytes per pixel; the filter type, an embedded ICC profile or an EXIF orientation made no difference.

- The PNG pre-shrink cutoff is now in decoded bytes, not pixels: 128 MB as RGBA, which is 32 MP of 8-bit and 16 MP of 16-bit pixels (4 and 8 bytes each). A 31 MP 16-bit PNG used to decode whole (240 MB of samples plus an 8-bit copy plus two clones for the resize) and now shrinks while decoding. `FASTPKG_PNG_SHRINK_MIN_BYTES` overrides it; `FASTPKG_PNG_SHRINK_MIN_PIXELS` still works and counts 8-bit pixels.
- Interlaced (Adam7) PNGs are shrunk while decoding too: pass rows are accumulated straight into the shrunk image (`next_interlaced_row`; the vendored `png` gained `Adam7Info::pass`/`line`), so neither the frame nor a de-interlaced copy is ever resident. The result equals the shrink of the same picture stored non-interlaced up to float rounding. Colour types, bit depths, tRNS/palette, `failOn` (interlaced stays all-or-nothing under `none`) and `limitInputPixels` behave as for the row-by-row path. Interlaced files under the cutoff keep the buffered decoder.
- 16-bit images lost their redundant full-size copies: a 16-bit PNG decoded for a resize no longer builds the 8-bit twin of its samples (`Image::from_rgba16_deferred`; any operation that needs it rebuilds it), the wide resize borrows the samples instead of cloning them and repacking them as pixel structs (28 bytes per pixel in flight, now 8), rotate/flip/extract/extend/affine on wide images move the buffer instead of copying it, and the PNG and raw16 writers borrow it too.
- Large vertical downscales are resized in bands of destination rows (about 24 MB of convolution intermediate instead of the whole source height, 152 MB for the 40 MP case above); the filter weights are those of the one-shot resize, so output differs by at most one level from float rounding.

### Accuracy

The decode-time PNG shrink is now an area average with exact fractional edge weights (no integer factor, no partial last block), taken to 4x the target size when the intermediate fits a 128 MiB budget, then 3.5x, 2.5x and 2x. It used to floor to an integer box factor that left 2x to 4x. PSNR in dB against an f64 Lanczos3 resample of the full-resolution pixels (3600x2400 pictures, `node bench/png-shrink-psnr.mjs`; exact = whole image resampled, old = 0.5.0 shrink):

| picture | downscale | exact | new shrink | old shrink | sharp |
|---|---|---|---|---|---|
| photo-like | 9x | 58.5 | 57.8 | 56.5 | 55.7 |
| photo-like | 18x | 58.7 | 57.8 | 53.7 | 54.9 |
| photo-like | 36x | 58.8 | 57.8 | 43.7 | 55.9 |
| textured, hard edges | 9x | 58.5 | 55.9 | 50.4 | 50.1 |
| textured, hard edges | 18x | 58.7 | 56.9 | 48.8 | 52.4 |
| textured, hard edges | 36x | 58.8 | 57.4 | 38.7 | 53.7 |
| flat graphics, thin lines, text | 9x | 39.0 | 39.4 | 32.6 | 32.6 |
| flat graphics, thin lines, text | 18x | 42.8 | 41.9 | 34.1 | 36.3 |
| flat graphics, thin lines, text | 36x | 45.8 | 45.1 | 32.6 | 40.2 |

The shrink is within about a dB of resampling the whole image and now beats sharp everywhere (before, a 36x downscale lost 13 to 20 dB). When the memory budget forces 2.5x or 2x (a 40 MP 16-bit file to 1200 px), it is still above sharp for ratios of 6x and up (graphics 35 to 39 dB at 2.5x against sharp's 32; headroom 3 is skipped, it resonates with the box and is erratically worse than 2.5) and ties with it at 2x. An axis that would shrink by less than 1.25x is not shrunk. Cost: the wider intermediate and the weighted average take a single-threaded 256 MP non-interlaced PNG to 1200 px from 1.2 s to 1.8 s and 104 MB to 173 MB; a 40 MP 16-bit file is faster (0.8 s to 0.5 s).

## 0.5.0

### Fixed

- WebP: the EXIF orientation is read from WebP files written by libvips/sharp (their EXIF chunk starts with the JPEG-style `Exif\0\0` prefix, which the container reader rejected), so `metadata().orientation`, `rotate()`/`autoOrient()` and `sharp(..., { autoOrient: true })` no longer leave a rotated WebP sideways.

### Fixed (continued)

- `concurrency(n)` now takes effect after the library has been used. Each image job runs on a rayon pool of `n` threads (sharp's meaning: threads per image) that is rebuilt on the first job after the value changes; it used to size rayon's global pool once and ignore later changes. `concurrency(0)` or no argument restores the default (the number of cores). Concurrent jobs still run in parallel whatever `n` is. Tiny concurrent jobs cost about 10% more at the default thread count (8 concurrent 200 px resizes: 2.3 ms against 2.1 ms).
- `versions.vips` reported `fastpkg 0.1.0`; it now follows `versions.fastpkg`, and a test pins both to the package version so a stale native binary fails the release tests.

### Memory and speed

- PNG downscales no longer lose accuracy or wall time below 32 MP: the row-by-row box pre-shrink now only applies to images of at least 32 MP (128 MiB as RGBA), where it saves memory; smaller files decode at full size and the parallel Lanczos resize sees every pixel again, as in 0.3.0 (a 4096x4096 PNG to 1024 px is 349 ms against 393 ms in 0.3.0, sharp 302 ms). `FASTPKG_PNG_SHRINK_MIN_PIXELS` overrides the threshold.
- A leading `rotate()` (auto-orient), `rotate(90/180/270)`, `flip()` or `flop()` no longer switches off shrink-on-load for the resize that follows: these operations only move pixels, so the decode hint now looks through them (width and height swapped for quarter turns) and the pre-shrink size travels with the image. A 256 MP PNG through `.rotate().resize(800)` peaks at about 83 MB instead of 2.2 GB (sharp: 308 MB), and JPEGs resized after `rotate()` use DCT scaling again. Added to `bench/mem.mjs` as `huge-png-rotate-800-jpeg`.
- PNG decodes at the default `failOn` no longer inflate the image data twice: the check that the zlib stream ends properly (a cut-short stream is an error at every level but `none`) ran a second full inflate on a helper thread, which hid in wall time but cost about a third more CPU. The decoder now sees the end of the stream, checksum included, in the bytes that follow the last row, and the second inflate only runs when that is inconclusive (checksums ignored, interlaced `failOn: 'none'`, data after the stream). Accept/reject verdicts are unchanged (whole `failOn` corpus identical). Process CPU for a 4096x4096 PNG through `.resize(1024).png()`: 631 ms to 481 ms per image at the default level, against 439-471 ms for `failOn: 'none'` (+44% before, +2-4% now); wall time is unchanged (339 ms). Uses a patched copy of the `png` crate (`vendor/png`, one added accessor, `Reader::zlib_stream_ended`).

## 0.4.0

### Memory

- PNG decoding is now sequential. Non-interlaced files are read row by row straight into the RGBA result (no intermediate full-frame buffer next to it, which held two copies of RGB and 16-bit images), and when a resize comes first and downscales by 4x or more the rows are box-averaged while they arrive (premultiplied for images with alpha, any colour type and bit depth), so only the shrunk image is resident. The factor leaves the Lanczos resize 2x headroom over the target (like libvips' integer pre-shrink, so PNG downscales of 4x and more are also closer to sharp's output than before: for example a 2400 px photo to 100 px is off by 0.3 instead of 2.0 levels on average); `fastShrinkOnLoad` does not apply to PNG. A 16384x16384 (256 MP), 800 KB flat PNG resized to 800 px: peak RSS 1.84 GB to 81 MB (sharp 272-307 MB), 2.2-2.5 s to 0.82 s (sharp 1.0-1.1 s). Interlaced PNGs still decode the whole frame, but 8-bit ones now expand to RGBA inside the decode buffer instead of next to it. `failOn` and `limitInputPixels` behave as before; `bench/mem.mjs` has the 256 MP case (`huge-png-800-png`, `huge-png-800-jpeg`).

### Colour management (sharp parity)

- Embedded ICC profiles are converted to sRGB while decoding, like sharp (perceptual intent, alpha untouched, pixels left alone for corrupt, unsupported or wrong-model profiles and for profiles equivalent to sRGB). `ignoreIcc: true` keeps the stored values, as do `keepIccProfile()` / `keepMetadata()` (the profile describes the pixels); `withMetadata()` converts and embeds sRGB, as in sharp. Untagged images only pay a header-level profile lookup.
- 16-bit RGB sources are converted to Display P3 (sharp's processing profile), through a 33-point tetrahedral lookup table as in lcms, so colours near the edge of the destination gamut match sharp (the exact transform differed by up to 17 levels there).
- CMYK/YCCK JPEGs decode to sRGB: Adobe-inverted values are un-inverted, an embedded CMYK profile is used when present and libvips' built-in CMYK profile otherwise; `metadata()` reports `space: 'cmyk'`, `channels: 4` and `chromaSubsampling: '4:4:4:4'` like sharp.
- Known differences: JPEG DCT-scale and non-integer resize factors differ slightly from libvips with or without a profile (see the resize tolerance in `tests/differential.test.mjs`); profiles of other colour models (Lab, ...) and 16-bit grey data are passed through unconverted, as in libvips.

### Fixed

- `failOn` is wired for every decoder instead of only JPEG truncation. A differential corpus (`tests/_failon-corpus.mjs`: seeded truncations, byte flips, zeroed and deleted ranges, chunk/segment deletion, bad lengths, CRC damage and trailing garbage over the fixtures and generated JPEG/PNG/WebP/GIF/TIFF/AVIF/SVG files, judged against sharp at all four levels) found 390 corrupt inputs that sharp rejects and fastpkg accepted in its 8,636 cases (four levels; mostly PNG, WebP, GIF, AVIF and JPEG); that count is now 4, one damaged AVIF container per level (see Known differences).
  - PNG: chunk CRCs are enforced from `failOn: 'error'` up and ignored below it; zlib checksums from `'truncated'` up. A zlib stream that ends early, a bad checksum and an invalid chunk type or length are errors; a data error after the last row, a missing or damaged `IEND`, and data after `IEND` are not, as in libpng. Interlaced files are all-or-nothing under `failOn: 'none'`.
  - JPEG: a fatal libjpeg error rejects the file at any level instead of falling through to a second decoder that accepted it; metadata keeps working on files with junk between marker segments.
  - WebP: a damaged container is rejected at any level (the check libvips makes through the demuxer) and damaged bitstreams are an error instead of being decoded by a lenient fallback.
  - GIF: a file cut inside a block is rejected unless `failOn: 'none'`, which returns the frames that survived; an invalid LZW code is fatal at every level unless it comes after the last pixel or inside a cut sub-block; any `GIF8` signature is accepted; the canvas follows libvips (frames extend a too-small logical screen, a screen over 2048 px or empty is ignored).
  - AVIF: bytes after the last complete box are ignored; a damaged AV1 tile is an error (rav1d's padding check is always on, as in libaom) and so is a picture whose size matches no `ispe` box.
- `sharp({ failOn: 'none' })` (options only, input written to the stream) is accepted.

### Known differences

- AVIF: a few damaged container boxes (`hdlr`, `infe`, `ipco`) are accepted where libheif rejects them, and some damaged `iloc`/`ftyp` fields are rejected where libheif decodes them.
- JPEG: a progressive file with an inconsistent scan script can fail in libvips even at `failOn: 'none'`. PNG: IHDR sizes beyond the decoder buffer limit are rejected here. GIF: under `'warning'` junk after a cut frame that parses as another frame is an error in libvips.
- WebP: some damaged lossless bitstreams decode here and fail in libvips.
- Tests: `tests/failon.test.mjs` runs a deterministic subset in `npm test` and the whole corpus with `FAILON_FULL=1`; the differences above are listed there as todos.

## 0.3.0

Output-size, speed and parity release. **PNG and WebP output bytes change** (see below); PNG bytes remain deterministic and independent of thread count, and are pinned by `tests/png-stability.test.mjs`. They are not libvips-identical.

### PNG

- The default output is no longer larger than sharp's: the encoder now picks deflate method and row filtering per image from a deterministic sample. Photographs, gradients and flat art use a new Huffman/run-length coder; graphics, screenshots and tiled art use zlib-rs with level N mapped to zlib-rs level N. On the benchmark corpus files are about equal to sharp's on graphics and often 3x or more smaller on photographs and gradients. Large decoded-JPEG-style photos use a flatter level ladder. `compressionLevel: 0` now writes stored (uncompressed) deflate blocks. `adaptiveFiltering` is honoured. Photographic and gradient images encode identically at levels 1-9. Large images are deflated in parallel in fixed blocks, so bytes never depend on the thread count.
- `png({ palette, quality, effort, colours, dither })` now behaves like sharp: any of the quantisation options implies `palette`, `colours` selects a 1/2/4/8-bit depth, images with at most 256 colours convert losslessly, alpha goes to `tRNS`. A built-in MIT quantiser (no libimagequant) is 5-80x faster than sharp's at comparable size and error.
- `iCCP` is written before `PLTE`.

### WebP

- Decoding uses libwebp: about 2-5x faster for lossy files and bit-identical to sharp. A resize at the start of a pipeline uses libwebp's scaled decode (resizing a 4000x3000 lossy WebP: about 60 ms instead of about 440 ms).
- Lossless: graphics, gradients and indexed-colour art are routed to libwebp by a ratio and colour-sampling classifier (fixes gradient art 4-18x larger than sharp's); palette-like images use libwebp at quality 100, noisy gradients escalate to method 0, escalation is capped at 8 MP for non-palette content, photographs keep the fast path.
- `nearLossless` no longer degrades when `quality` is low.
- Bundled libwebp updated from 1.3.1 to 1.6.0 (includes the fix for CVE-2023-4863).

### Operations

- `blur()` matches libvips `gaussblur` bit for bit (integer precision, `minAmplitude`, premultiplied RGBA); `precision: 'float'` is within one level on about 0.05% of pixels. `blur()` / `blur(true)` is sharp's 3x3 mild blur. `sharpen()` is sharp's 3x3 convolution (bit-exact); `sharpen({ sigma, m1, m2, x1, y2, y3 })` runs libvips' LAB-lightness unsharp mask (within one grey level).
- `rotate(angle)` and `affine()` blend border pixels into `background` by coverage like libvips, interpolate images with alpha premultiplied, and match sharp to within one level; `affine({ interpolator: 'bicubic' | 'nearest' })` is honoured.
- Resize kernels `lanczos2`, `mks2013` and `mks2021` use their own filters instead of Lanczos3.
- `dilate()`/`erode()` keep metadata and grey images.

### Performance

- Gaussian blur 2-8x faster; `sharpen()`, mild `blur()`, `convolve`, `dilate`/`erode`, `trim` (parallel), `composite` (parallel, table-driven), `extend`, arbitrary-angle `rotate`/`affine`, `bandbool` faster with byte-identical output; LAB `sharpen({sigma})` about 2.5x faster with identical output.
- Single-thread `photo png 2000 -> png`: about 520 ms to about 76 ms (sharp about 223 ms).
- On glibc Linux the addon pins glibc's mmap threshold at 4 MiB (`FASTPKG_MALLOC_TUNING=0` disables), cutting steady-state RSS about 30-45% on 1024^2 RGBA workloads at about +10% CPU on 4 MiB-frame pipelines; musl, macOS and Windows are unchanged.

### Packaging and typings

- `require('@fastpkg/image/package.json')` works.
- TypeScript declarations mirror sharp's (`sharp.Metadata`, `sharp.OutputInfo`, `sharp.Sharp`, `sharp.ResizeOptions`, ... ) for `import`, `require` and `import x = require()` under node16, nodenext and bundler resolution. `npm run test:types` compiles the same sources against fastpkg and sharp. `index.d.cts` and `THIRD_PARTY_NOTICES.md` ship in the package.
- `bench/mem.mjs` (memory harness) added.

### Known limits

- Options accepted but without effect: JPEG `trellisQuantisation`/`quantisationTable`/`overshootDeringing`; WebP `smartDeblock`/`minSize`/`mixed`; GIF `colours`/`dither`/`reuse`/frame-error options; AVIF `lossless`/`bitdepth`/`tune`.
- Single-thread PNG encode of flat art and screenshots at levels 1-6 is 1.1-2x slower than sharp (files within 0.3% of sharp's size); with the default thread pool most cases are faster.

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
