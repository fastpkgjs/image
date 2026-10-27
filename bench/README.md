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

## PNG memory and accuracy tools

Big fixtures are generated, never committed (`bench/.cache/`):

```bash
node bench/gen-huge-png.mjs out.png --size 6325 --depth 16            # 40 MP 16-bit RGB, deflates to a few hundred KB
node bench/gen-huge-png.mjs out.png --size 16384 --interlace         # 256 MP Adam7
node bench/gen-huge-png.mjs out.png --size 6325 --depth 16 --content noise   # incompressible worst case
node bench/peak-rss.mjs out.png --pipeline rotate-1200-jpeg          # peak RSS (VmHWM) of one run, fastpkg and sharp, fresh process each
node bench/mem.mjs --scenarios png16-40mp-rotate-1200-jpeg,huge-adam7-800-jpeg --conc 1
node bench/png-shrink-psnr.mjs --old /path/to/0.5.0/package-dir      # PSNR against a float Lanczos3 reference
```

`peak-rss.mjs` honours `FASTPKG_PKG=<package dir>` to A/B another build. `png-shrink-psnr.mjs` runs the decode-time shrink forced on and off (`FASTPKG_PNG_SHRINK_MIN_BYTES`) next to sharp, on photo-like, textured and hard-edge (thin lines, text-like strokes) pictures.
