//! PNG writer: row filtering, deterministic (optionally block-parallel) zlib and chunk assembly.
//!
//! Everything here is a pure function of (pixels, parameters): the filter choice looks only at
//! pixel data, parallel work is split on fixed boundaries and re-assembled in order, so the
//! bytes never depend on the number of threads or on scheduling.

#![allow(clippy::needless_range_loop)]

use flate2::{Compress, Compression, FlushCompress, Status};
use rayon::prelude::*;

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
    let strategy = if level == 0 {
        Strategy::None
    } else {
        match p.adaptive {
            Some(true) => Strategy::Adaptive,
            Some(false) => Strategy::None,
            None => choose_strategy(&passes, bpp, level, p.color_type == 3),
        }
    };
    let mut filtered = Vec::with_capacity(passes.iter().map(|(rb, r, _)| (rb + 1) * r).sum());
    for (rb, rows, data) in &passes {
        filter_into(data, *rb, *rows, bpp, strategy, &mut filtered);
    }
    let z = zlib(&filtered, level);

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

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i16, b as i16, c as i16);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn filter_row(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    let n = cur.len();
    let head = bpp.min(n);
    match kind {
        0 => out.copy_from_slice(cur),
        1 => {
            out[..head].copy_from_slice(&cur[..head]);
            for i in head..n {
                out[i] = cur[i].wrapping_sub(cur[i - bpp]);
            }
        }
        2 => {
            for i in 0..n {
                out[i] = cur[i].wrapping_sub(prev[i]);
            }
        }
        3 => {
            for i in 0..head {
                out[i] = cur[i].wrapping_sub(prev[i] >> 1);
            }
            for i in head..n {
                let avg = ((cur[i - bpp] as u16 + prev[i] as u16) >> 1) as u8;
                out[i] = cur[i].wrapping_sub(avg);
            }
        }
        _ => {
            for i in 0..head {
                out[i] = cur[i].wrapping_sub(prev[i]);
            }
            for i in head..n {
                out[i] = cur[i].wrapping_sub(paeth(cur[i - bpp], prev[i], prev[i - bpp]));
            }
        }
    }
}

#[inline]
fn residual_cost(row: &[u8]) -> u64 {
    let mut total = 0u64;
    for chunk in row.chunks(4096) {
        let mut acc = 0u32;
        for &v in chunk {
            acc += (v as i8).unsigned_abs() as u32;
        }
        total += acc as u64;
    }
    total
}

/// Filters rows `[start, end)` of `data` into `out` (`(row_bytes + 1) * (end - start)` bytes).
fn filter_range(
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
            Strategy::Adaptive => {
                let mut best_kind = 0u8;
                let mut best_cost = residual_cost(cur);
                for kind in 1..5u8 {
                    let buf = &mut scratch[(kind - 1) as usize];
                    filter_row(kind, cur, prev, bpp, buf);
                    let cost = residual_cost(buf);
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
        }
    }
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

/// Picks `None` or `Adaptive` by trial-compressing a deterministic sample of rows.
fn choose_strategy(
    passes: &[(usize, usize, &[u8])],
    bpp: usize,
    level: u8,
    indexed: bool,
) -> Strategy {
    // Sample budget: about 256 KiB (1 MiB from level 7) of raw rows per candidate, taken as eight
    // evenly spaced bands. Small images are probed whole.
    // Strong levels are slow anyway and are asked for by callers who want the smallest file.
    let sample_bytes: usize = if level >= 7 { 1 << 20 } else { 256 << 10 };
    const BANDS: usize = 8;
    let (row_bytes, rows, data) = match passes.iter().max_by_key(|(rb, r, _)| rb * r) {
        Some(&(rb, r, d)) => (rb, r, d),
        None => return Strategy::None,
    };
    if rows == 0 || row_bytes == 0 {
        return Strategy::None;
    }
    let sample_rows = (sample_bytes / row_bytes).clamp(32, 1024);
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
    let (none_rows, adaptive_rows) =
        rayon::join(|| sample(Strategy::None), || sample(Strategy::Adaptive));
    // Photographic content: unfiltered bytes are close to random (collision sum under n^2/32)
    // while filtering concentrates them near zero (at least 4x more collisions). That is
    // decided from the byte histograms alone, without compressing anything.
    let n = none_rows.len() as u64;
    let (c_none, c_adaptive) = (collision_sum(&none_rows), collision_sum(&adaptive_rows));
    // (Palette indices are not intensities: a low collision count there says nothing.)
    if !indexed && c_none * 32 < n * n && c_adaptive > c_none * 4 {
        return Strategy::Adaptive;
    }
    let (none, adaptive) = rayon::join(
        || deflate_raw(&none_rows, level, None, true).len(),
        || deflate_raw(&adaptive_rows, level, None, true).len(),
    );
    // Unfiltered rows are what libvips writes and are cheaper to produce, so adaptive filtering
    // has to win clearly (2%) to be chosen.
    if adaptive * 100 < none * 98 {
        Strategy::Adaptive
    } else {
        Strategy::None
    }
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

/// Raw deflate of `data`. `dict` primes the window (previous 32 KiB of the full stream);
/// `last` ends the stream, otherwise the block is sync-flushed (byte aligned) so the next one
/// can be appended.
fn deflate_raw(data: &[u8], level: u8, dict: Option<&[u8]>, last: bool) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(level as u32), false);
    if let Some(d) = dict {
        // Cannot fail for a fresh raw-deflate stream.
        let _ = c.set_dictionary(d);
    }
    let mut out = Vec::with_capacity(data.len() / 2 + 1024);
    let flush = if last {
        FlushCompress::Finish
    } else {
        FlushCompress::Sync
    };
    let mut consumed = 0usize;
    loop {
        if out.capacity() - out.len() < 4096 {
            out.reserve(data.len() / 4 + 4096);
        }
        let before = c.total_in();
        let status = c.compress_vec(&data[consumed..], &mut out, flush);
        consumed += (c.total_in() - before) as usize;
        match status {
            Ok(Status::StreamEnd) | Err(_) => break,
            Ok(_) => {
                if !last && consumed == data.len() && out.capacity() > out.len() {
                    break;
                }
            }
        }
    }
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

/// Complete zlib stream. Level 0 is stored blocks; levels above 0 are zlib-rs at that level,
/// deflated as fixed-size independent blocks when the input is large.
pub fn zlib(data: &[u8], level: u8) -> Vec<u8> {
    let flg = match level {
        0 | 1 => 0x01,
        2..=5 => 0x5e,
        6 => 0x9c,
        _ => 0xda,
    };
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.extend_from_slice(&[0x78, flg]);
    if level == 0 {
        stored_blocks(data, &mut out);
        out.extend_from_slice(&adler32(data).to_be_bytes());
        return out;
    }
    if data.len() < SPLIT_MIN {
        out.extend_from_slice(&deflate_raw(data, level, None, true));
        out.extend_from_slice(&adler32(data).to_be_bytes());
        return out;
    }
    let count = data.len().div_ceil(SPLIT_BLOCK);
    let parts: Vec<(Vec<u8>, u32)> = (0..count)
        .into_par_iter()
        .map(|i| {
            let start = i * SPLIT_BLOCK;
            let end = (start + SPLIT_BLOCK).min(data.len());
            let dict = (i > 0).then(|| &data[start.saturating_sub(32768)..start]);
            (
                deflate_raw(&data[start..end], level, dict, i + 1 == count),
                adler32(&data[start..end]),
            )
        })
        .collect();
    let mut adler = 1u32;
    for (i, (bytes, a)) in parts.iter().enumerate() {
        out.extend_from_slice(bytes);
        let len = SPLIT_BLOCK.min(data.len() - i * SPLIT_BLOCK);
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
            let data = sample(n);
            for level in 0..=9u8 {
                let z = zlib(&data, level);
                let mut back = Vec::new();
                flate2::read::ZlibDecoder::new(&z[..])
                    .read_to_end(&mut back)
                    .unwrap_or_else(|e| panic!("n={n} level={level}: {e}"));
                assert_eq!(back, data, "n={n} level={level}");
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
