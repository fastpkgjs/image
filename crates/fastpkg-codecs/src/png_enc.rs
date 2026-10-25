//! PNG writer: row filtering, deterministic (optionally block-parallel) zlib and chunk assembly.
//!
//! Everything here is a pure function of (pixels, parameters): the filter choice looks only at
//! pixel data, parallel work is split on fixed boundaries and re-assembled in order, so the
//! bytes never depend on the number of threads or on scheduling.

#![allow(clippy::needless_range_loop)]

use rayon::prelude::*;
use zlib_rs::{
    Deflate, DeflateConfig, DeflateFlush, Method as ZMethod, Status, Strategy as ZStrategy,
};

// Level ladder (zlib levels, as in libpng/sharp): 0 stored, 1-3 fast, 4-6 default (6 unless
// overridden), 7-9 strong.

/// Rows filtered per rayon task (fixed so that the split is independent of the thread count).
const ROWS_PER_TASK: usize = 32;

/// Filtered data larger than this is deflated as independent blocks of `SPLIT_BLOCK` bytes (each
/// primed with the previous 32 KiB as dictionary) and concatenated into one zlib stream.
const SPLIT_MIN: usize = 3 << 19;
const SPLIT_BLOCK: usize = 1 << 19;

/// Maximum IDAT payload per chunk.
const IDAT_CHUNK: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Strategy {
    /// Filter type 0 on every row.
    None,
    /// Per row, the filter with the smallest sum of absolute (signed) residuals.
    Adaptive,
}

pub struct Params<'a> {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    /// PNG colour type: 0 gray, 2 rgb, 3 indexed, 4 gray+alpha, 6 rgba.
    pub color_type: u8,
    pub palette: Option<&'a [u8]>,
    pub trns: Option<&'a [u8]>,
    pub interlace: bool,
    pub level: u8,
    /// `Some(true)` forces per-row adaptive filtering, `Some(false)` forces none, `None` picks
    /// the better of the two by trial-compressing a sample of rows.
    pub adaptive: Option<bool>,
}

fn channels(color_type: u8) -> usize {
    match color_type {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    }
}

pub fn crc_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
}

/// Packs 8-bit palette indices into `depth`-bit samples (rows padded to whole bytes).
pub fn pack_indices(indices: &[u8], width: usize, depth: u8) -> Vec<u8> {
    if depth >= 8 {
        return indices.to_vec();
    }
    let per_byte = 8 / depth as usize;
    let row_bytes = width.div_ceil(per_byte);
    let rows = indices.len() / width.max(1);
    let mut out = vec![0u8; row_bytes * rows];
    for (src, dst) in indices
        .chunks_exact(width)
        .zip(out.chunks_exact_mut(row_bytes))
    {
        for (x, &v) in src.iter().enumerate() {
            let shift = 8 - depth as usize * (x % per_byte + 1);
            dst[x / per_byte] |= v << shift;
        }
    }
    out
}

pub fn encode(p: &Params, samples: &[u8], out: &mut Vec<u8>) {
    let bits_pp = channels(p.color_type) * p.bit_depth as usize;
    let bpp = bits_pp.div_ceil(8).max(1);
    let (w, h) = (p.width as usize, p.height as usize);
    let row_bytes = (w * bits_pp).div_ceil(8);
    debug_assert_eq!(samples.len(), row_bytes * h);

    // (row_bytes, rows, data) per pass.
    let owned;
    let passes: Vec<(usize, usize, &[u8])> = if p.interlace {
        owned = adam7_passes(samples, w, h, bpp);
        owned
            .iter()
            .map(|(rb, r, d)| (*rb, *r, d.as_slice()))
            .collect()
    } else {
        vec![(row_bytes, h, samples)]
    };

    let level = p.level.min(9);
    let plan = if level == 0 {
        Plan {
            strategy: Strategy::None,
            method: Method::Stored,
        }
    } else {
        choose_plan(&passes, bpp, level, p.color_type == 3, p.adaptive)
    };
    let direct = !p.interlace && plan.strategy == Strategy::None && plan.method != Method::Stored;
    let filtered;
    let src = if direct {
        // Unfiltered rows go straight to the compressor.
        Source::Plain {
            data: samples,
            row_bytes,
            rows: h,
        }
    } else {
        let mut f = Vec::with_capacity(passes.iter().map(|(rb, r, _)| (rb + 1) * r).sum());
        for (rb, rows, data) in &passes {
            filter_into(data, *rb, *rows, bpp, plan.strategy, &mut f);
        }
        filtered = f;
        Source::Flat(&filtered)
    };
    let z = zlib(&src, plan.method);

    out.reserve(z.len() + z.len() / 4096 * 12 + 512);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&p.width.to_be_bytes());
    header.extend_from_slice(&p.height.to_be_bytes());
    header.extend_from_slice(&[p.bit_depth, p.color_type, 0, 0, p.interlace as u8]);
    crc_chunk(out, b"IHDR", &header);
    if let Some(palette) = p.palette {
        crc_chunk(out, b"PLTE", palette);
    }
    if let Some(trns) = p.trns {
        crc_chunk(out, b"tRNS", trns);
    }
    for part in z.chunks(IDAT_CHUNK) {
        crc_chunk(out, b"IDAT", part);
    }
    crc_chunk(out, b"IEND", &[]);
}

fn adam7_passes(samples: &[u8], w: usize, h: usize, bpp: usize) -> Vec<(usize, usize, Vec<u8>)> {
    const PASSES: [(usize, usize, usize, usize); 7] = [
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ];
    let mut result = Vec::new();
    for (x0, y0, dx, dy) in PASSES {
        if x0 >= w || y0 >= h {
            continue;
        }
        let pw = (w - x0).div_ceil(dx);
        let ph = (h - y0).div_ceil(dy);
        let mut data = Vec::with_capacity(pw * bpp * ph);
        for y in (y0..h).step_by(dy) {
            for k in 0..pw {
                let start = (y * w + x0 + k * dx) * bpp;
                data.extend_from_slice(&samples[start..start + bpp]);
            }
        }
        result.push((pw * bpp, ph, data));
    }
    result
}

// ---------------------------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------------------------

/// Sum of absolute (signed) residuals: the usual heuristic for picking a row filter.
#[inline(always)]
fn cost_of(out: &[u8]) -> u64 {
    let mut total = 0u64;
    for chunk in out.chunks(8192) {
        let mut acc = 0u32;
        for &v in chunk {
            acc += (v as i8).unsigned_abs() as u32;
        }
        total += acc as u64;
    }
    total
}

#[inline(always)]
fn paeth_px(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i16, b as i16, c as i16);
    let pa = (ib - ic).abs();
    let pb = (ia - ic).abs();
    let pc = (ia + ib - ic - ic).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Filters `cur` with `kind` (1..=4) into `out` and returns the residual cost. The loops run
/// over zipped slices (reading only unfiltered rows, so no dependency between iterations) and
/// vectorise.
#[inline(always)]
fn filter_row(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u64 {
    let n = cur.len();
    let head = bpp.min(n);
    match kind {
        1 => {
            out[..head].copy_from_slice(&cur[..head]);
            for ((o, &x), &a) in out[head..].iter_mut().zip(&cur[head..]).zip(cur) {
                *o = x.wrapping_sub(a);
            }
        }
        2 => {
            for ((o, &x), &b) in out.iter_mut().zip(cur).zip(prev) {
                *o = x.wrapping_sub(b);
            }
        }
        3 => {
            for ((o, &x), &b) in out[..head].iter_mut().zip(&cur[..head]).zip(prev) {
                *o = x.wrapping_sub(b >> 1);
            }
            for (((o, &x), &a), &b) in out[head..]
                .iter_mut()
                .zip(&cur[head..])
                .zip(cur)
                .zip(&prev[head..])
            {
                *o = x.wrapping_sub(((a as u16 + b as u16) >> 1) as u8);
            }
        }
        _ => {
            for ((o, &x), &b) in out[..head].iter_mut().zip(&cur[..head]).zip(prev) {
                *o = x.wrapping_sub(b);
            }
            for ((((o, &x), &a), &b), &c) in out[head..]
                .iter_mut()
                .zip(&cur[head..])
                .zip(cur)
                .zip(&prev[head..])
                .zip(prev)
            {
                *o = x.wrapping_sub(paeth_px(a, b, c));
            }
        }
    }
    cost_of(out)
}

/// Picks the filter for one row (smallest residual sum, ties to the lower type) and writes the
/// filter byte plus the filtered row to `dst`.
#[inline(always)]
fn filter_adaptive_row(
    cur: &[u8],
    prev: &[u8],
    bpp: usize,
    scratch: &mut [Vec<u8>],
    dst: &mut [u8],
) {
    // A row equal to its predecessor is all zeros under Up: nothing can beat that.
    if cur == prev {
        dst[0] = if cur.iter().all(|&v| v == 0) { 0 } else { 2 };
        dst[1..].fill(0);
        return;
    }
    let mut best_kind = 0u8;
    let mut best_cost = cost_of(cur);
    // Up first; when it leaves almost nothing (flat or vertically smooth rows) the others are
    // not worth computing. Evaluation order (and so tie-breaking) is fixed: 0, 2, 1, 3, 4.
    for kind in [2u8, 1, 3, 4] {
        if best_cost == 0 || (kind == 1 && best_kind == 2 && best_cost * 16 <= cur.len() as u64) {
            break;
        }
        let buf = &mut scratch[(kind - 1) as usize];
        let cost = filter_row(kind, cur, prev, bpp, buf);
        if cost < best_cost {
            best_cost = cost;
            best_kind = kind;
        }
    }
    dst[0] = best_kind;
    if best_kind == 0 {
        dst[1..].copy_from_slice(cur);
    } else {
        dst[1..].copy_from_slice(&scratch[(best_kind - 1) as usize]);
    }
}

#[inline(always)]
fn filter_range_impl(
    data: &[u8],
    row_bytes: usize,
    bpp: usize,
    start: usize,
    end: usize,
    strategy: Strategy,
    out: &mut [u8],
) {
    let zeros = vec![0u8; row_bytes];
    let mut scratch: Vec<Vec<u8>> = if strategy == Strategy::Adaptive {
        (0..4).map(|_| vec![0u8; row_bytes]).collect()
    } else {
        Vec::new()
    };
    for (n, y) in (start..end).enumerate() {
        let cur = &data[y * row_bytes..(y + 1) * row_bytes];
        let prev = if y == 0 {
            &zeros[..]
        } else {
            &data[(y - 1) * row_bytes..y * row_bytes]
        };
        let dst = &mut out[n * (row_bytes + 1)..(n + 1) * (row_bytes + 1)];
        match strategy {
            Strategy::None => {
                dst[0] = 0;
                dst[1..].copy_from_slice(cur);
            }
            Strategy::Adaptive => filter_adaptive_row(cur, prev, bpp, &mut scratch, dst),
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn filter_range_avx2(
    data: &[u8],
    row_bytes: usize,
    bpp: usize,
    start: usize,
    end: usize,
    strategy: Strategy,
    out: &mut [u8],
) {
    filter_range_impl(data, row_bytes, bpp, start, end, strategy, out);
}

/// Filters rows `[start, end)` of `data` into `out` (`(row_bytes + 1) * (end - start)` bytes).
/// The bytes written do not depend on the instruction set used.
fn filter_range(
    data: &[u8],
    row_bytes: usize,
    bpp: usize,
    start: usize,
    end: usize,
    strategy: Strategy,
    out: &mut [u8],
) {
    #[cfg(target_arch = "x86_64")]
    if strategy == Strategy::Adaptive && std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2 (checked just above); the callee only runs safe code.
        return unsafe { filter_range_avx2(data, row_bytes, bpp, start, end, strategy, out) };
    }
    filter_range_impl(data, row_bytes, bpp, start, end, strategy, out);
}

fn filter_into(
    data: &[u8],
    row_bytes: usize,
    rows: usize,
    bpp: usize,
    strategy: Strategy,
    dst: &mut Vec<u8>,
) {
    let base = dst.len();
    dst.resize(base + (row_bytes + 1) * rows, 0);
    let region = &mut dst[base..];
    if rows == 0 || row_bytes == 0 {
        return;
    }
    let work = |(task, chunk): (usize, &mut [u8])| {
        let start = task * ROWS_PER_TASK;
        let end = (start + ROWS_PER_TASK).min(rows);
        filter_range(data, row_bytes, bpp, start, end, strategy, chunk);
    };
    if strategy == Strategy::None || rows * row_bytes < (1 << 18) {
        region
            .chunks_mut((row_bytes + 1) * ROWS_PER_TASK)
            .enumerate()
            .for_each(work);
    } else {
        region
            .par_chunks_mut((row_bytes + 1) * ROWS_PER_TASK)
            .enumerate()
            .for_each(work);
    }
}

/// How the filtered stream is deflated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// Stored blocks (level 0).
    Stored,
    /// Entropy coding only (Huffman, plus distance-1 run tokens where they pay off): for data
    /// without exploitable repeats, such as adaptively filtered photographs, or made of long
    /// runs, where it is both faster and no larger than LZ77.
    Entropy,
    /// LZ77 + Huffman at this zlib-rs level (1-9).
    Lz(u8),
}

struct Plan {
    strategy: Strategy,
    method: Method,
}

/// Internal LZ77 effort (zlib-rs level) for each requested level (index = level; 0 is stored).
/// It only applies to content where string matching wins: graphics, screenshots, tiled or
/// dithered art. Photographs and run-dominated art are entropy coded whatever the level (the
/// strongest levels cannot beat that on them, and cost 10-100x more). zlib-rs level N lands
/// within 0.3% of the reference encoder's (libvips + zlib-ng) size at its level N on the
/// graphics measured, so the ladder is the identity (levels 8-9 stay the "smaller" end).
fn lz_level(level: u8) -> u8 {
    level.min(9)
}

/// Highest zlib-rs level per requested level when LZ77 output stays above 15% of the input:
/// there the strong levels spend 2-5x the time of level 2-3 for 3-8% in size.
const SLOW_LZ_CAP: [u8; 10] = [0, 1, 2, 2, 2, 2, 3, 3, 4, 5];

/// Chooses the row-filter strategy and the deflate method by trial-compressing a deterministic
/// sample of rows (eight evenly spaced bands, 128 KiB per candidate). A cheap method (Huffman
/// only or RLE) is preferred unless LZ77 is at least 3% smaller on the sample.
fn choose_plan(
    passes: &[(usize, usize, &[u8])],
    bpp: usize,
    level: u8,
    indexed: bool,
    forced: Option<bool>,
) -> Plan {
    const SAMPLE_BYTES: usize = 128 << 10;
    const BANDS: usize = 8;
    let lz = lz_level(level);
    let fallback = |strategy| Plan {
        strategy,
        method: Method::Lz(lz),
    };
    let (row_bytes, rows, data) = match passes.iter().max_by_key(|(rb, r, _)| rb * r) {
        Some(&(rb, r, d)) => (rb, r, d),
        None => return fallback(Strategy::None),
    };
    if rows == 0 || row_bytes == 0 {
        return fallback(Strategy::None);
    }
    let sample_rows = (SAMPLE_BYTES / row_bytes).clamp(32, 1024);
    let band = (sample_rows / BANDS).max(4);
    let ranges: Vec<(usize, usize)> = if rows <= band * BANDS {
        vec![(0, rows)]
    } else {
        (0..BANDS)
            .map(|i| {
                let start = (rows - band) * i / (BANDS - 1);
                (start, start + band)
            })
            .collect()
    };
    let sample = |strategy: Strategy| -> Vec<u8> {
        let mut buf = Vec::new();
        for &(s, e) in &ranges {
            let base = buf.len();
            buf.resize(base + (row_bytes + 1) * (e - s), 0);
            filter_range(data, row_bytes, bpp, s, e, strategy, &mut buf[base..]);
        }
        buf
    };
    // Probe one candidate: entropy coding and LZ77 (capped at level 2) on the sample.
    let probe = |rows: &[u8]| -> (Method, usize, usize) {
        let entropy = crate::png_huff::size(rows);
        // Near-random bytes (over 7 bits each after entropy coding) with no repeated 8-byte
        // strings: matching cannot pay off, so skip the (slow, on such data) LZ77 probe.
        let lz_size = if entropy * 100 > rows.len() * 85 && !has_repeats(rows) {
            usize::MAX / 128
        } else {
            deflate_raw(
                &Source::Flat(rows),
                0,
                rows.len(),
                Method::Lz(lz.min(2)),
                true,
            )
            .len()
        };
        (Method::Entropy, entropy, lz_size)
    };
    let big = row_bytes * rows > (1 << 20);
    // Smaller is better. The cheap method wins ties; where LZ77 is itself fast (it finds
    // plenty of matches, but not so many that the data is nearly all runs) the cheap method has
    // to be 5% smaller to be worth its own cost.
    let pick =
        |(cheap, cheap_size, lz_size): (Method, usize, usize), raw: usize| -> (Method, u64) {
            // LZ77 output between 2% and 40% of the input.
            let fast_lz = lz_size * 50 >= raw && lz_size * 5 < raw * 2;
            // At the fast levels LZ77 on the sample underestimates what the reference gets.
            let margin: u64 = match (fast_lz, lz) {
                (true, 1..=2) => 90,
                (true, _) => 95,
                _ => 100,
            };
            if cheap_size as u64 * 100 <= lz_size as u64 * margin {
                (cheap, cheap_size as u64)
            } else if big && lz_size * 20 >= raw * 3 {
                // Large and poorly compressible (15% or more of the input even with matching,
                // e.g. decoded JPEG photographs): the hash-chain levels cost several times
                // more for a few percent, so the ladder is flatter.
                (
                    Method::Lz(lz.min(SLOW_LZ_CAP[level.min(9) as usize])),
                    lz_size as u64,
                )
            } else {
                (Method::Lz(lz), lz_size as u64)
            }
        };
    match forced {
        Some(true) => {
            let rows = sample(Strategy::Adaptive);
            Plan {
                strategy: Strategy::Adaptive,
                method: pick(probe(&rows), rows.len()).0,
            }
        }
        Some(false) => {
            let rows = sample(Strategy::None);
            Plan {
                strategy: Strategy::None,
                method: pick(probe(&rows), rows.len()).0,
            }
        }
        None => {
            let (none_rows, adaptive_rows) =
                rayon::join(|| sample(Strategy::None), || sample(Strategy::Adaptive));
            // Photographic content: unfiltered bytes are close to random (collision sum under
            // n^2/32) while filtering concentrates them near zero (at least 4x more
            // collisions). That is decided from the byte histograms alone. (Palette indices are
            // not intensities: a low collision count there says nothing.)
            let n = none_rows.len() as u64;
            let (c_none, c_adaptive) = (collision_sum(&none_rows), collision_sum(&adaptive_rows));
            if !indexed && c_none * 32 < n * n && c_adaptive > c_none * 4 {
                return Plan {
                    strategy: Strategy::Adaptive,
                    method: pick(probe(&adaptive_rows), adaptive_rows.len()).0,
                };
            }
            let ((m_none, s_none), (m_adaptive, s_adaptive)) = rayon::join(
                || pick(probe(&none_rows), none_rows.len()),
                || pick(probe(&adaptive_rows), adaptive_rows.len()),
            );
            // Unfiltered rows are cheaper to produce, so adaptive filtering has to win
            // clearly (2%) to be chosen.
            // (LZ77 on a few bands of rows misses repeats between rows: its size is
            // overestimated, so filtering has to win by more against it.)
            let needed = if matches!(m_none, Method::Lz(_)) {
                90
            } else {
                98
            };
            if s_adaptive * 100 < s_none * needed {
                // Filtering already beats the unfiltered reference by half: spend no more than
                // level 6 on the string matching (large images only).
                let huge = row_bytes * rows > (4 << 20);
                let method = match m_adaptive {
                    Method::Lz(l) if huge && l > 6 && s_none * 2 >= s_adaptive * 3 => Method::Lz(6),
                    m => m,
                };
                Plan {
                    strategy: Strategy::Adaptive,
                    method,
                }
            } else {
                Plan {
                    strategy: Strategy::None,
                    method: m_none,
                }
            }
        }
    }
}

/// True when at least 1 in 20 positions of `data` starts an 8-byte string seen earlier in it
/// (a direct-mapped hash probe, about 1 ns per byte). Tiled textures, dithering patterns and
/// anything else LZ77 is good at pass; noise and photographs do not.
fn has_repeats(data: &[u8]) -> bool {
    const BITS: u32 = 13;
    let mut table = vec![0u64; 1 << BITS];
    let mut hits = 0usize;
    for w in data.windows(8) {
        let key = u64::from_le_bytes(w.try_into().unwrap());
        let slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - BITS)) as usize;
        if table[slot] == key {
            hits += 1;
        } else {
            table[slot] = key;
        }
    }
    hits * 20 >= data.len()
}

/// Sum of squared byte-value counts: an integer collision measure of the order-0 distribution
/// (n^2 for constant data, n^2/256 for uniformly random data). Integer-only so that decisions
/// based on it are identical on every platform.
fn collision_sum(data: &[u8]) -> u64 {
    let mut counts = [0u64; 256];
    for &v in data {
        counts[v as usize] += 1;
    }
    counts.iter().map(|&c| c * c).sum()
}

// ---------------------------------------------------------------------------------------------
// zlib
// ---------------------------------------------------------------------------------------------

fn adler32(data: &[u8]) -> u32 {
    zlib_rs::adler32::adler32(1, data)
}

fn adler32_combine(a1: u32, a2: u32, len2: usize) -> u32 {
    zlib_rs::adler32::adler32_combine(a1, a2, len2 as u64)
}

/// The byte stream to compress: filtered rows already laid out in memory, or unfiltered rows
/// with an implied filter byte of 0 in front of each (which saves materialising a copy).
#[derive(Clone, Copy)]
pub enum Source<'a> {
    Flat(&'a [u8]),
    Plain {
        data: &'a [u8],
        row_bytes: usize,
        rows: usize,
    },
}

impl<'a> Source<'a> {
    fn len(&self) -> usize {
        match *self {
            Source::Flat(d) => d.len(),
            Source::Plain {
                row_bytes, rows, ..
            } => (row_bytes + 1) * rows,
        }
    }

    /// Calls `f` on consecutive pieces covering stream bytes `[start, end)`.
    fn pieces(&self, start: usize, end: usize, mut f: impl FnMut(&[u8])) {
        match *self {
            Source::Flat(d) => f(&d[start..end]),
            Source::Plain {
                data, row_bytes, ..
            } => {
                let stride = row_bytes + 1;
                let mut pos = start;
                while pos < end {
                    let (row, offset) = (pos / stride, pos % stride);
                    let row_end = ((row + 1) * stride).min(end);
                    if offset == 0 {
                        f(&[0]);
                        pos += 1;
                        if pos >= row_end {
                            continue;
                        }
                    }
                    let from = row * row_bytes + (pos - row * stride - 1);
                    let to = from + (row_end - pos);
                    f(&data[from..to]);
                    pos = row_end;
                }
            }
        }
    }

    fn bytes(&self, start: usize, end: usize) -> std::borrow::Cow<'a, [u8]> {
        match *self {
            Source::Flat(d) => d[start..end].into(),
            _ => {
                let mut v = Vec::with_capacity(end - start);
                self.pieces(start, end, |p| v.extend_from_slice(p));
                v.into()
            }
        }
    }

    fn adler(&self, start: usize, end: usize) -> u32 {
        let mut a = 1u32;
        self.pieces(start, end, |p| a = zlib_rs::adler32::adler32(a, p));
        a
    }
}

/// Feeds `input` to the compressor, appending output to `out`; with a flush other than
/// `NoFlush` runs until the flush is complete.
fn feed(c: &mut Deflate, out: &mut Vec<u8>, input: &[u8], flush: DeflateFlush, reserve: usize) {
    let mut consumed = 0usize;
    loop {
        if out.capacity() - out.len() < 4096 {
            out.reserve(reserve + 4096);
        }
        let used = out.len();
        out.resize(out.capacity(), 0);
        let (before_in, before_out) = (c.total_in(), c.total_out());
        let status = c.compress(&input[consumed..], &mut out[used..], flush);
        consumed += (c.total_in() - before_in) as usize;
        out.truncate(used + (c.total_out() - before_out) as usize);
        match status {
            Ok(Status::StreamEnd) | Err(_) => break,
            Ok(_) => {
                if consumed == input.len() && out.capacity() > out.len() {
                    break;
                }
            }
        }
    }
}

/// Raw deflate of stream bytes `[start, end)`. For the LZ methods the previous 32 KiB of the
/// stream prime the window; `last` ends the stream, otherwise the part is sync-flushed (byte
/// aligned) so the next one can be appended.
fn deflate_raw(src: &Source, start: usize, end: usize, method: Method, last: bool) -> Vec<u8> {
    let before = start.checked_sub(1).map(|i| src.bytes(i, start)[0]);
    if method == Method::Entropy {
        let data = src.bytes(start, end);
        let mut out = Vec::new();
        crate::png_huff::deflate(&data, before, last, &mut out);
        return out;
    }
    let level = match method {
        Method::Lz(l) => l.clamp(1, 9) as i32,
        _ => 1,
    };
    let mut c = Deflate::new_with_config(DeflateConfig {
        level,
        method: ZMethod::Deflated,
        window_bits: -15,
        mem_level: 8,
        strategy: ZStrategy::Default,
    });
    if start > 0 {
        // Cannot fail for a fresh raw-deflate stream.
        let _ = c.set_dictionary(&src.bytes(start.saturating_sub(32768), start));
    }
    let len = end - start;
    let mut out: Vec<u8> = Vec::with_capacity(len / 2 + 1024);
    // Rows (and their filter bytes) are small: gather them so that the compressor sees large
    // inputs, which is markedly faster than many tiny calls.
    let mut stage: Vec<u8> = Vec::new();
    src.pieces(start, end, |piece| {
        if stage.is_empty() && piece.len() >= (1 << 15) {
            feed(&mut c, &mut out, piece, DeflateFlush::NoFlush, len / 4);
            return;
        }
        stage.extend_from_slice(piece);
        if stage.len() >= (1 << 15) {
            feed(&mut c, &mut out, &stage, DeflateFlush::NoFlush, len / 4);
            stage.clear();
        }
    });
    if !stage.is_empty() {
        feed(&mut c, &mut out, &stage, DeflateFlush::NoFlush, len / 4);
    }
    let flush = if last {
        DeflateFlush::Finish
    } else {
        DeflateFlush::SyncFlush
    };
    feed(&mut c, &mut out, &[], flush, len / 4);
    out
}

fn stored_blocks(data: &[u8], out: &mut Vec<u8>) {
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
        return;
    }
    let mut chunks = data.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        out.push(chunks.peek().is_none() as u8);
        out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        out.extend_from_slice(chunk);
    }
}

/// Complete zlib stream. Large inputs are deflated as fixed-size blocks, in parallel, each
/// primed with the previous 32 KiB (LZ methods) and concatenated, so the bytes are identical
/// at any thread count.
pub fn zlib(src: &Source, method: Method) -> Vec<u8> {
    let total = src.len();
    let flg = match method {
        Method::Lz(2..=5) => 0x5e,
        Method::Lz(6) => 0x9c,
        Method::Lz(7..) => 0xda,
        _ => 0x01,
    };
    let mut out = Vec::with_capacity(total / 2 + 64);
    out.extend_from_slice(&[0x78, flg]);
    if method == Method::Stored {
        let data = src.bytes(0, total);
        stored_blocks(&data, &mut out);
        out.extend_from_slice(&adler32(&data).to_be_bytes());
        return out;
    }
    if total < SPLIT_MIN {
        out.extend_from_slice(&deflate_raw(src, 0, total, method, true));
        out.extend_from_slice(&src.adler(0, total).to_be_bytes());
        return out;
    }
    let count = total.div_ceil(SPLIT_BLOCK);
    let parts: Vec<(Vec<u8>, u32)> = (0..count)
        .into_par_iter()
        .map(|i| {
            let start = i * SPLIT_BLOCK;
            let end = (start + SPLIT_BLOCK).min(total);
            (
                deflate_raw(src, start, end, method, i + 1 == count),
                src.adler(start, end),
            )
        })
        .collect();
    let mut adler = 1u32;
    for (i, (bytes, a)) in parts.iter().enumerate() {
        out.extend_from_slice(bytes);
        let len = SPLIT_BLOCK.min(total - i * SPLIT_BLOCK);
        adler = if i == 0 {
            *a
        } else {
            adler32_combine(adler, *a, len)
        };
    }
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample(n: usize) -> Vec<u8> {
        let mut state = 12345u32;
        (0..n)
            .map(|i| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                if i % 7 < 4 {
                    (i / 7) as u8
                } else {
                    (state >> 24) as u8
                }
            })
            .collect()
    }

    #[test]
    fn zlib_round_trips_at_every_level_and_size() {
        for n in [
            0usize,
            1,
            100,
            70_000,
            SPLIT_MIN + 12_345,
            SPLIT_BLOCK * 5 + 7,
        ] {
            let mut data = sample(n);
            if n % 2 == 1 {
                // Long runs, crossing the block boundaries.
                data = (0..n).map(|i| ((i / 3001) % 5) as u8).collect();
            }
            let methods = [Method::Stored, Method::Entropy]
                .into_iter()
                .chain((1..=9u8).map(Method::Lz));
            for level in methods {
                let z = zlib(&Source::Flat(&data), level);
                let mut back = Vec::new();
                flate2::read::ZlibDecoder::new(&z[..])
                    .read_to_end(&mut back)
                    .unwrap_or_else(|e| panic!("n={n} method={level:?}: {e}"));
                assert_eq!(back, data, "n={n} method={level:?}");
            }
        }
    }

    #[test]
    fn plain_source_matches_materialised_rows() {
        for (row_bytes, rows) in [
            (1usize, 5usize),
            (7, 3),
            (1000, 40),
            (4096, 400),
            (30_001, 61),
        ] {
            let data = sample(row_bytes * rows);
            let mut flat = Vec::new();
            for row in data.chunks(row_bytes) {
                flat.push(0);
                flat.extend_from_slice(row);
            }
            let plain = Source::Plain {
                data: &data,
                row_bytes,
                rows,
            };
            let total = flat.len();
            assert_eq!(plain.len(), total);
            for (a, b) in [
                (0, total),
                (1, total),
                (3, total - 1),
                (total / 3, total / 2),
            ] {
                if a < b {
                    assert_eq!(
                        &plain.bytes(a, b)[..],
                        &flat[a..b],
                        "{row_bytes}x{rows} {a}..{b}"
                    );
                }
            }
            for method in [Method::Entropy, Method::Lz(1), Method::Lz(6)] {
                let z = zlib(&plain, method);
                if method == Method::Entropy {
                    assert_eq!(z, zlib(&Source::Flat(&flat), method), "{row_bytes}x{rows}");
                }
                let mut back = Vec::new();
                flate2::read::ZlibDecoder::new(&z[..])
                    .read_to_end(&mut back)
                    .unwrap_or_else(|e| panic!("{row_bytes}x{rows} {method:?}: {e}"));
                assert_eq!(back, flat, "{row_bytes}x{rows} {method:?}");
            }
        }
    }

    #[test]
    fn adler_matches_reference_and_combines() {
        fn reference(data: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &v in data {
                a = (a + v as u32) % 65521;
                b = (b + a) % 65521;
            }
            (b << 16) | a
        }
        let data = sample(200_000);
        assert_eq!(adler32(&data), reference(&data));
        let (a, b) = data.split_at(77_777);
        assert_eq!(
            adler32_combine(adler32(a), adler32(b), b.len()),
            reference(&data)
        );
    }

    #[test]
    fn encodes_decode_back_for_every_layout() {
        let layouts: [(u8, u8); 11] = [
            (0, 8),
            (2, 8),
            (4, 8),
            (6, 8),
            (0, 16),
            (2, 16),
            (6, 16),
            (3, 1),
            (3, 2),
            (3, 4),
            (3, 8),
        ];
        for (color_type, depth) in layouts {
            for (w, h) in [(1u32, 1u32), (5, 3), (70, 40), (3, 90)] {
                let bits = channels(color_type) * depth as usize;
                let row_bytes = (w as usize * bits).div_ceil(8);
                let mut samples = sample(row_bytes * h as usize);
                if color_type == 3 {
                    // Keep indices inside the palette (padding bits of sub-byte rows stay as is).
                    let max = (1u16 << depth) as u8;
                    if depth < 8 {
                        samples = vec![0; row_bytes * h as usize];
                        for (i, v) in samples.iter_mut().enumerate() {
                            *v = (i as u8).wrapping_mul(37);
                        }
                    } else {
                        for v in &mut samples {
                            *v %= max.max(1);
                        }
                    }
                }
                let palette: Vec<u8> = (0..(1usize << depth.min(8)) * 3).map(|i| i as u8).collect();
                for interlace in [false, true] {
                    if interlace && depth < 8 {
                        continue;
                    }
                    for level in [0u8, 1, 6, 9] {
                        for adaptive in [None, Some(true), Some(false)] {
                            let mut out = Vec::new();
                            encode(
                                &Params {
                                    width: w,
                                    height: h,
                                    bit_depth: depth,
                                    color_type,
                                    palette: (color_type == 3).then_some(&palette[..]),
                                    trns: None,
                                    interlace,
                                    level,
                                    adaptive,
                                },
                                &samples,
                                &mut out,
                            );
                            let mut reader = png::Decoder::new(std::io::Cursor::new(&out))
                                .read_info()
                                .unwrap_or_else(|e| panic!("{color_type}/{depth} {w}x{h}: {e}"));
                            let mut buf = vec![0; reader.output_buffer_size().unwrap()];
                            let info = reader.next_frame(&mut buf).unwrap_or_else(|e| {
                                panic!("{color_type}/{depth} {w}x{h} i{interlace} L{level}: {e}")
                            });
                            assert_eq!(
                                &buf[..info.buffer_size()],
                                &samples[..],
                                "{color_type}/{depth} {w}x{h} i{interlace} L{level} {adaptive:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn packed_indices() {
        assert_eq!(
            pack_indices(&[1, 0, 1, 1, 0, 0, 0, 1, 1], 9, 1),
            vec![0b1011_0001, 0b1000_0000]
        );
        assert_eq!(pack_indices(&[3, 2, 1, 0], 4, 2), vec![0b1110_0100]);
        assert_eq!(pack_indices(&[15, 1, 2], 3, 4), vec![0xf1, 0x20]);
    }
}
