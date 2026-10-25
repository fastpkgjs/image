# Benchmarks

Run:

```bash
npm run fixtures
node bench/run.mjs
```

`bench/run.mjs` creates `fixtures/large.jpg` (4000x3000) on first run, disables sharp's cache, sets both engines to concurrency 1, warms each scenario, and reports the median and p95 of end-to-end Node calls.

Current machine used for the result table in the root README:

- Linux x86_64 (Debian 13), 12 vCPU
- AMD EPYC-Rome

- Node 24
- Rust 1.99.1
- sharp 0.35.5 (libvips 8.18.7, mozjpeg, zlib-ng, libwebp)
- @fastpkg/image 0.2.0 (MozJPEG JPEG codec, Rust PNG/WebP/AVIF/JXL/QOI/GIF pipeline)

The benchmark is intentionally end-to-end: it includes the JavaScript builder, N-API crossing, decode, pixel operations, encode, and Buffer/file output. It is not a codec microbenchmark.

Caveats:

- JPEG/PNG quality and output size are not normalized. The speed table uses the same quality/compression settings but does not claim equal file size.
- Run on a shared VM: ratios are more meaningful than absolute milliseconds, and individual rows move by ±10% between runs.
- `stats` includes a full decode and a full-image scan for both engines.
- The photographic lossless WebP scenario uses the pure-Rust lossless encoder (files are larger than sharp's: smaller-is-not-claimed); the graphic lossless WebP scenario is routed to libwebp (method 1) and lands within a few percent of sharp's size; the lossy WebP scenario uses libwebp in both engines.
- This is a development benchmark, not a statistically controlled release benchmark. Use a dedicated runner for publication.
