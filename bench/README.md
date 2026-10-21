# Benchmarks

Run:

```bash
npm run fixtures
node bench/run.mjs
```

`bench/run.mjs` creates `fixtures/large.jpg` (4000x3000) on first run, disables sharp's cache, sets both engines to concurrency 1, warms each scenario, and reports the median and p95 of end-to-end Node calls.

Current machine used for the result table in the root README:

- Windows 11, x86_64
- Intel Core i5-12450H, 8 cores / 12 threads
- 16 GB RAM
- Node 26.4.0
- Rust 1.99.1
- sharp 0.35.5 (libvips 8.18.7, mozjpeg, zlib-ng, libwebp)
- @fastpkg/image 0.1.0 (MozJPEG JPEG codec, Rust PNG/WebP/AVIF/JXL/QOI/GIF pipeline)

The benchmark is intentionally end-to-end: it includes the JavaScript builder, N-API crossing, decode, pixel operations, encode, and Buffer/file output. It is not a codec microbenchmark.

Caveats:

- JPEG/PNG quality and output size are not normalized. The speed table uses the same quality/compression settings but does not claim equal file size.
- The CPU is a laptop part and may thermally throttle. Ratios are more meaningful than absolute milliseconds.
- `stats` includes a full decode and a full-image scan for both engines.
- The WebP scenario is lossless for both engines.
- This is a development benchmark, not a statistically controlled release benchmark. Use a dedicated runner for publication.
