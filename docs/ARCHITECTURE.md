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

Blur, sharpen, median, morphology, point operations, channel operations, and composites run in `fastpkg-ops`. Most loops use rayon over pixels or rows. `stats` uses one parallel pass for channel min/max/mean/stdev, luminance entropy, edge sharpness, opacity, and dominant colour, instead of one pass per metric.

## Streaming

The JS `Sharp` object extends `node:stream.Duplex`. Writable input chunks are collected and the terminal native pipeline runs in `_final`. `toStream()` provides a readable wrapper. This is chunked I/O, not incremental pixel decode for every codec. True row/tile streaming is reserved for the next codec iteration.

## Error handling

`FastpkgError` is mapped to N-API errors. Worker panics from `spawn_blocking` are converted to JavaScript errors rather than unwinding through the FFI boundary. Dimensions are checked against `limitInputPixels` before decode.

## Build

`scripts/build.ps1` locates Visual Studio with `vswhere`, imports `vcvars64.bat`, prepends the bundled NASM directory, and invokes `@napi-rs/cli`. The addon is built with LTO and one codegen unit.

## Current performance limits

- JPEG uses MozJPEG/libjpeg-turbo through the `mozjpeg` crate, with a pure-Rust fallback. This is a codec dependency only; libvips is still not used.
- AVIF encode uses ravif and is slower than SVT-AV1/aom at high effort.
- WebP output supports lossy and lossless; lossy uses the pure-Rust `oxideav-webp` encoder.
- `stats` performs a full decode and is intentionally not a header-only operation.
- Blur uses a separable f32 implementation with an SSE4.1 path on x86-64. Median and morphology use bounded scratch buffers/separable passes, but large trim/filter operations still buffer the whole image.

