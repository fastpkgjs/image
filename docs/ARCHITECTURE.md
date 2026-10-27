# Architecture

## Data flow

```
JS chain builder
  -> PipelineSpec (one object)
  -> N-API async call
  -> header probe
  -> decode with planner hint
  -> operation graph
  -> encode
  -> external Buffer / file / stream
```

The JavaScript wrapper is a sharp-compatible builder. Chained methods only mutate a plain spec. `toBuffer`, `toFile`, `metadata`, `stats`, and `probe` are the only native calls.

## Crates

| Crate | Responsibility |
|---|---|
| `fastpkg-core` | Image representation, colours, pipeline spec, operation validation, limits, format detection |
| `fastpkg-codecs` | PNG/JPEG/WebP/AVIF/JXL/QOI/GIF/SVG/raw decode and encode; JPEG DCT scaling |
| `fastpkg-ops` | Resize, orientation, filters, colour ops, compositing, channel ops, statistics |
| `fastpkg-meta` | EXIF, XMP, ICC extraction; metadata enrichment |
| `fastpkg-stream` | Execution orchestration, mmap input, atomic file writes, probe/metadata/stats entry points |
| `fastpkg-napi` | N-API bindings, async worker dispatch, Buffer ownership |

## Image representation

Internally an image is interleaved RGBA8 with width, height, format, colour space, depth, orientation, and optional metadata payloads. PNG/raw 16-bit input additionally carries an optional RGBA16 buffer; geometry operations preserve it, while point/filter operations currently use the 8-bit representation. Codecs convert to this representation at the boundary.

## Zero-copy boundaries

- File input: `memmap2` read-only mapping.
- Async Buffer input: napi-rs `Buffer` owns a reference to the JS buffer; Rust borrows it on the worker thread. The JS caller must not mutate the buffer while the promise is pending.
- Output: Rust produces a `Vec<u8>`; `Buffer::from(Vec<u8>)` transfers it to Node without copying.
- The encoded output is the only large allocation on the return path. Internal operations may allocate new pixel buffers when an operation is not in-place.

## Header-only fast path

`probe` and `metadata` do not decode pixels. They use:
- Fast format-specific parsers for JPEG, PNG, WebP, GIF, QOI, AVIF, and JXL.
- `imagesize` fallback for other containers.
- EXIF orientation is parsed separately; full EXIF/XMP/ICC extraction runs only for `metadata`.

The returned header is used to reject oversized input before allocation and to choose the JPEG DCT scale.

## JPEG shrink-on-load

If the first resize in the pipeline needs a smaller image, `fastpkg-stream` computes the target and passes it to `fastpkg-codecs`. JPEG input is decoded with MozJPEG at a DCT scale close to the target (3/8, 1/2, 1/4, ...), using `fastShrinkOnLoad` to permit the nearest lower scale. A pure-Rust `jpeg-decoder` path is the fallback. This is why the first resize does not always pay for a full-resolution JPEG decode.

## PNG shrink-on-load

PNG rows are decoded one at a time (`fastpkg-codecs`, `decode_png_stream`), so a large file never exists as a raw frame next to its RGBA copy. When the first resample is a downscale and the decoded image is big, the rows are also averaged into the shrunk image as they arrive and only that image is resident (`png_shrink.rs`):

- **When.** The decoded size as RGBA is at least 128 MB: 32 MP of 8-bit pixels, 16 MP of 16-bit pixels (the cutoff is in bytes because memory is). Below it the image decodes whole and the parallel resampler sees every pixel. `FASTPKG_PNG_SHRINK_MIN_BYTES` overrides it (tests set it to 0); `FASTPKG_PNG_SHRINK_MIN_PIXELS` is the older spelling, counted in 8-bit pixels.
- **What.** An area average with exact fractional edge weights from the source size to an intermediate size, so the resampler that follows sees an exactly aligned, exactly scaled image. Colour is averaged premultiplied when there is alpha. The intermediate is 4x the target size on each axis when its memory fits a 128 MiB budget, then 3.5x, 2.5x, 2x (against a full-resolution float Lanczos reference this is within about a dB of resampling the whole image, and several dB better than libvips' integer pre-shrink on thin lines; `bench/png-shrink-psnr.mjs` prints the numbers). An axis that would shrink by less than 1.25x is left alone. Leading `rotate`/`flip`/`autoOrient`/`flatten`/`greyscale` are looked through (`fastpkg-stream`), the resize still computes its target from the full-resolution size (`Image::pre_shrink`).
- **Interlaced files.** Adam7 rows arrive pass by pass, so the average is accumulated in place at the shrunk size (four `f32` per output pixel) from the pass rows (`Reader::next_interlaced_row`, with the `pass`/`line` accessors added to the vendored `png`), and the result equals the non-interlaced shrink up to float rounding. Interlaced files below the cutoff, or that cannot shrink, still use the buffered decoder. Interlaced files are all-or-nothing under `failOn: 'none'`.
- **16-bit.** 16-bit PNGs keep wide `u16` samples next to an 8-bit twin (`Image::wide` / `data`). A 16-bit PNG decoded for a resize skips the twin (`Image::from_rgba16_deferred`); `fastpkg_ops::apply` rebuilds it before any operation that reads it. Resizing borrows the wide samples instead of cloning and repacking them, and large vertical downscales run in bands so the convolution intermediate stays near 24 MB.

## Resize and filters

`fast_image_resize` provides separable convolution kernels with runtime-dispatched SIMD. The plan maps sharp kernel names to FIR kernels:

| sharp | Fastpkg |
|---|---|
| `nearest` | nearest |
| `linear`/`bilinear` | bilinear |
| `cubic` | Catmull-Rom |
| `mitchell` | Mitchell |
| `lanczos2` | Lanczos3 (approximation) |
| `lanczos3` | Lanczos3 |
| `mks2013`/`mks2021` | Lanczos3 (approximation) |

Blur, sharpen, median, morphology, point operations, channel operations, and composites run in `fastpkg-ops`. Most loops use rayon over pixels or rows. `sharpen({ sigma, ... })` (libvips' LAB-lightness unsharp mask) converts rows of pixels to CIELAB and back in blocks of eight with the same arithmetic as the scalar reference (AVX2 build selected at run time; only the cube root differs, to one ulp, and the output is identical for all 2^24 input colours), blurs the quantised lightness plane with the integer mask and applies the curve in the same pass as the way back. `stats` uses one parallel pass for channel min/max/mean/stdev, luminance entropy, edge sharpness, opacity, and dominant colour, instead of one pass per metric.

## Streaming

The JS `Sharp` object extends `node:stream.Duplex`. Writable input chunks are collected and the terminal native pipeline runs in `_final`. `toStream()` provides a readable wrapper. This is chunked I/O, not incremental pixel decode for every codec. True row/tile streaming is reserved for the next codec iteration.

## Error handling

`FastpkgError` is mapped to N-API errors. Worker panics from `spawn_blocking` are converted to JavaScript errors rather than unwinding through the FFI boundary. Dimensions are checked against `limitInputPixels` before decode.

## Build

`scripts/build.ps1` locates Visual Studio with `vswhere`, imports `vcvars64.bat`, prepends the bundled NASM directory, and invokes `@napi-rs/cli`. The addon is built with LTO and one codegen unit.

## Current performance limits

- JPEG uses MozJPEG/libjpeg-turbo through the `mozjpeg` crate, with a pure-Rust fallback. This is a codec dependency only; libvips is still not used.
- AVIF encode uses ravif and is slower than SVT-AV1/aom at high effort.
- WebP output supports lossy and lossless; lossy and near-lossless use libwebp (BSD-3, compiled from the C sources bundled in `libwebp-sys` with the `cc` crate: no CMake or system library), with `effort` mapped to libwebp `method`, plus `quality`, `alphaQuality`, `smartSubsample` (sharp YUV) and `preset`; EXIF/ICC/XMP are attached through libwebp's mux. Lossless defaults to the pure-Rust `image-webp` encoder (about 25x faster than libwebp, but files 10-60% larger on photographs and 3-10x larger on graphics). Palette-like images (a deterministic 4096-pixel sample holds at most 1024 distinct colours: flat art, screenshots, icons, indexed and dithered images) skip the fast coder entirely and go straight to libwebp lossless at quality 100 (method 2 up to 0.5 MP, where it is about 10% smaller on small outputs, method 1 up to 16 MP, method 0 beyond; method 2 is otherwise at most 0.5% smaller than method 1 and 20% slower). For everything else the fast output is challenged only when it is below 30% of the raw pixel bytes (photographs sit above 40%; smooth gradients and smooth photographs between 10% and 30% shrink 15-30%) or the image has at most 16384 pixels, and never above 8 MP (libwebp costs about 0.25 s per megapixel on this content): images up to 160 Ki pixels simply run libwebp methods 0 and 1 (quality 100) and keep the smallest result; larger ones encode eight evenly spaced bands of rows (about 64 Ki pixels) with the fast coder and with both libwebp settings, escalate only if the better libwebp size is at least 15% below the fast one, and then use method 0 (about 2.5x cheaper than method 1, and on whole noisy gradients often smaller) unless method 1 is 15% smaller on the sample. A synthetic noisy gradient (1200x900) goes from 274 KB with the fast coder to 198 KB (sharp: 228 KB) in about 0.3 s (sharp: 0.68 s). Photographs fail the tests and pay one 4096-pixel scan. Setting `effort` explicitly always uses libwebp lossless (`effort` -> method, `quality` -> its size/speed trade-off, `exact` -> keep colours under transparent pixels). EXIF/ICC/XMP are attached through the libwebp mux for every lossless path. Encoder calls are wrapped in `catch_unwind` and images over 16383 px are rejected. `nearLossless` passes `quality` as libwebp's `near_lossless` level and pins the lossless effort at 75, as libvips does.
- WebP decoding uses libwebp (`WebPDecode`, fancy upsampling, RGBA output; bit-identical to sharp). When a resize is the first operation and shrinks by 2x or more, libwebp's scaled decode produces the target size (or 2x of it with `fastShrinkOnLoad: false`) and `Image.pre_shrink` carries the full-resolution size so the final dimensions are still computed from it. Dimensions are checked against the pixel limit from the header before the output buffer is allocated. Animated and damaged files fall back to `image-webp`, which decides the error (first frame only for animations). The decode call is wrapped in `catch_unwind`.
- `stats` performs a full decode and is intentionally not a header-only operation.
- Blur uses a separable f32 implementation with an SSE4.1 path on x86-64. Median and morphology use bounded scratch buffers/separable passes, but large trim/filter operations still buffer the whole image.

