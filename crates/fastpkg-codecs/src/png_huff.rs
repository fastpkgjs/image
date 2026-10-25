//! Entropy-coding-only deflate: dynamic-Huffman blocks of literals plus, where they pay off,
//! run-length matches (distance 1).
//!
//! Adaptively filtered photographs have almost no exploitable repeats, so LZ77 spends its time
//! on hash chains that rarely hit, while a plain entropy coder reaches the same size (usually a
//! little smaller, because no bits go to rare length/distance symbols) at several times the
//! speed. Filtered flat art is dominated by long runs of identical bytes, which the run
//! tokens handle without any match search. The output is a pure function of the input bytes.

/// Literal/length alphabet: 256 literals, end of block, 29 length codes.
const SYMBOLS: usize = 286;
/// A block is tokenised for runs only when at least 1/N of its bytes lie in flat chunks.
const RUN_SHARE: usize = 8;

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Length code index (0-28) for each match length 3..=258 (indexed by `length - 3`).
const LEN_INDEX: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut len = 3;
    while len <= 258 {
        let mut i = 28;
        while LEN_BASE[i] as usize > len {
            i -= 1;
        }
        table[len - 3] = i as u8;
        len += 1;
    }
    table
};

#[inline(always)]
fn length_index(len: usize) -> usize {
    LEN_INDEX[len - 3] as usize
}

/// Input bytes per Huffman block.
pub const BLOCK: usize = 1 << 17;

const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Code lengths for `freq` limited to `limit` bits. Symbols with zero frequency get length 0.
/// At least two symbols always get a code (as zlib does), so the code is complete.
fn code_lengths(freq: &[u32], limit: u8, lens: &mut [u8]) {
    let mut f = [(0u64, 0usize); SYMBOLS + 2];
    let mut m = 0;
    for (sym, &c) in freq.iter().enumerate() {
        if c > 0 {
            f[m] = (c as u64, sym);
            m += 1;
        }
    }
    for spare in [0usize, 1] {
        if m < 2 && !f[..m].iter().any(|x| x.1 == spare) {
            f[m] = (1, spare);
            m += 1;
        }
    }
    lens.iter_mut().for_each(|l| *l = 0);
    let f = &mut f[..m];
    let mut weight = [0u64; 2 * SYMBOLS];
    let mut parent = [0u16; 2 * SYMBOLS];
    let mut depth = [0u8; 2 * SYMBOLS];
    loop {
        f.sort_unstable();
        for (i, &(w, _)) in f.iter().enumerate() {
            weight[i] = w;
        }
        let (mut leaf, mut node, mut next) = (0usize, m, m);
        for _ in 0..m - 1 {
            let mut pair = [0usize; 2];
            for slot in &mut pair {
                if leaf < m && (node >= next || weight[leaf] <= weight[node]) {
                    *slot = leaf;
                    leaf += 1;
                } else {
                    *slot = node;
                    node += 1;
                }
            }
            weight[next] = weight[pair[0]] + weight[pair[1]];
            parent[pair[0]] = next as u16;
            parent[pair[1]] = next as u16;
            next += 1;
        }
        let root = next - 1;
        depth[root] = 0;
        for i in (0..root).rev() {
            depth[i] = depth[parent[i] as usize].saturating_add(1);
        }
        let deepest = depth[..m].iter().copied().max().unwrap_or(0);
        if deepest <= limit {
            for (i, &(_, s)) in f.iter().enumerate() {
                lens[s] = depth[i];
            }
            return;
        }
        // Too deep: flatten the distribution and retry (deterministic).
        for e in f.iter_mut() {
            e.0 = e.0.div_ceil(2);
        }
    }
}

/// Canonical codes, bit-reversed for LSB-first emission: `codes[s] = (bits, length)`.
fn canonical(lens: &[u8], codes: &mut [(u16, u8)]) {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    for (s, &l) in lens.iter().enumerate() {
        if l == 0 {
            codes[s] = (0, 0);
            continue;
        }
        let c = next[l as usize];
        next[l as usize] += 1;
        codes[s] = (c.reverse_bits() >> (16 - l as u32), l);
    }
}

struct Bits<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits<'_> {
    #[inline(always)]
    fn put(&mut self, value: u32, len: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += len;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    fn align(&mut self) {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.acc = 0;
        self.n = 0;
    }
}

/// Run-length coded code-length sequence: `(symbol, extra bits value)` with symbols 0-18.
fn rle_lengths(lens: &[u8], out: &mut Vec<(u8, u8)>) {
    let n = lens.len();
    let mut i = 0;
    while i < n {
        let l = lens[i];
        let mut run = 1;
        while i + run < n && lens[i + run] == l {
            run += 1;
        }
        i += run;
        if l == 0 {
            while run >= 11 {
                let r = run.min(138);
                out.push((18, (r - 11) as u8));
                run -= r;
            }
            if run >= 3 {
                out.push((17, (run - 3) as u8));
                run = 0;
            }
            for _ in 0..run {
                out.push((0, 0));
            }
        } else {
            out.push((l, 0));
            run -= 1;
            while run >= 3 {
                let r = run.min(6);
                out.push((16, (r - 3) as u8));
                run -= r;
            }
            for _ in 0..run {
                out.push((l, 0));
            }
        }
    }
}

fn histogram(block: &[u8]) -> [u32; SYMBOLS] {
    let mut h = [[0u32; 256]; 4];
    let mut chunks = block.chunks_exact(4);
    for c in &mut chunks {
        h[0][c[0] as usize] += 1;
        h[1][c[1] as usize] += 1;
        h[2][c[2] as usize] += 1;
        h[3][c[3] as usize] += 1;
    }
    for &v in chunks.remainder() {
        h[0][v as usize] += 1;
    }
    let mut total = [0u32; SYMBOLS];
    for i in 0..256 {
        total[i] = h[0][i] + h[1][i] + h[2][i] + h[3][i];
    }
    total[256] = 1;
    total
}

/// Bytes lying in 16-byte aligned chunks of one repeated value: a cheap, branch-free proxy for
/// the amount of data that run tokens could cover (runs of 32+ always contain such a chunk).
fn flat_bytes(block: &[u8]) -> usize {
    16 * block
        .chunks_exact(16)
        .map(|c| (c.iter().fold(0u8, |acc, &v| acc | (v ^ c[0])) == 0) as usize)
        .sum::<usize>()
}

/// Finds the distance-1 runs of `block`: a run is three or more bytes equal to the byte before
/// them (`prev` is the byte before the block, if any), cut into tokens of at most 258. Returns
/// `(position, length)` pairs in order; everything between them is coded as literals. Updates
/// the literal histogram `freq` (initially that of the whole block) and returns the extra
/// bits the run tokens need.
fn find_runs(
    block: &[u8],
    prev: Option<u8>,
    freq: &mut [u32; SYMBOLS],
    runs: &mut Vec<(u32, u16)>,
) -> u64 {
    let n = block.len();
    let mut extra_bits = 0u64;
    runs.clear();
    let mut push = |pos: usize, len: usize, value: u8, freq: &mut [u32; SYMBOLS]| {
        let idx = length_index(len);
        runs.push((pos as u32, len as u16));
        freq[value as usize] -= len as u32;
        freq[257 + idx] += 1;
        extra_bits += LEN_EXTRA[idx] as u64 + 1;
    };
    // Cuts the run of `left` bytes starting at `pos` into tokens; a remainder of one or two
    // bytes stays literal.
    let mut cut = |mut pos: usize, mut left: usize, value: u8, freq: &mut [u32; SYMBOLS]| {
        while left >= 3 {
            let len = left.min(258);
            push(pos, len, value, freq);
            pos += len;
            left -= len;
        }
    };
    let mut i = 0usize;
    while i < n {
        // Candidate: block[i] equals its predecessor.
        let before = if i == 0 { prev } else { Some(block[i - 1]) };
        if before != Some(block[i]) {
            // Skip ahead eight bytes at a time while no byte equals its predecessor.
            if i >= 1 {
                while i + 8 <= n {
                    let cur = u64::from_le_bytes(block[i..i + 8].try_into().unwrap());
                    let before = u64::from_le_bytes(block[i - 1..i + 7].try_into().unwrap());
                    let x = cur ^ before;
                    // Zero byte in `x` <=> a byte equal to its predecessor.
                    let zero = x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080;
                    if zero != 0 {
                        i += (zero.trailing_zeros() / 8) as usize;
                        break;
                    }
                    i += 8;
                }
                if i >= n {
                    break;
                }
                if block[i] != block[i - 1] {
                    // Fell out of the window loop without a candidate (tail or false positive
                    // from the carry in the zero-byte test): step on.
                    i += 1;
                    continue;
                }
            } else {
                i += 1;
                continue;
            }
        }
        let value = block[i];
        let mut j = i + 1;
        let splat = u64::from(value) * 0x0101_0101_0101_0101;
        while j + 8 <= n {
            let diff = u64::from_le_bytes(block[j..j + 8].try_into().unwrap()) ^ splat;
            if diff != 0 {
                j += (diff.trailing_zeros() / 8) as usize;
                break;
            }
            j += 8;
        }
        while j < n && block[j] == value {
            j += 1;
        }
        cut(i, j - i, value, freq);
        i = j + 1;
    }
    extra_bits
}

/// Everything needed to emit one block.
struct Tables {
    codes: [(u16, u8); SYMBOLS],
    cl_lens: [u8; 19],
    cl_codes: [(u16, u8); 19],
    seq: Vec<(u8, u8)>,
    nlit: usize,
    hclen: usize,
    /// Total size in bits of header + coded data.
    bits: u64,
}

fn tables(freq: &[u32; SYMBOLS], extra_bits: u64) -> Tables {
    let mut lens = [0u8; SYMBOLS];
    code_lengths(freq, 15, &mut lens);
    let mut codes = [(0u16, 0u8); SYMBOLS];
    canonical(&lens, &mut codes);
    let nlit = (1..=SYMBOLS)
        .rev()
        .find(|&n| lens[n - 1] != 0)
        .unwrap_or(257)
        .max(257);
    // The distance alphabet has one used code; like zlib, declare two codes of one bit.
    let mut seq = Vec::with_capacity(SYMBOLS + 2);
    rle_lengths(&lens[..nlit], &mut seq);
    rle_lengths(&[1, 1], &mut seq);
    let mut cl_freq = [0u32; 19];
    for &(s, _) in &seq {
        cl_freq[s as usize] += 1;
    }
    let mut cl_lens = [0u8; 19];
    code_lengths(&cl_freq, 7, &mut cl_lens);
    let mut cl_codes = [(0u16, 0u8); 19];
    canonical(&cl_lens, &mut cl_codes);
    let mut hclen = 19;
    while hclen > 4 && cl_lens[CL_ORDER[hclen - 1]] == 0 {
        hclen -= 1;
    }
    let mut bits = 3 + 5 + 5 + 4 + 3 * hclen as u64 + extra_bits;
    for &(s, _) in &seq {
        bits += cl_lens[s as usize] as u64
            + match s {
                16 => 2,
                17 => 3,
                18 => 7,
                _ => 0,
            };
    }
    for s in 0..SYMBOLS {
        bits += freq[s] as u64 * lens[s] as u64;
    }
    Tables {
        codes,
        cl_lens,
        cl_codes,
        seq,
        nlit,
        hclen,
        bits,
    }
}

/// How one block is coded: plain literals, or literals and runs.
struct BlockPlan {
    tables: Tables,
    /// Run tokens (`position`, `length`) when chosen.
    runs: Option<Vec<(u32, u16)>>,
}

fn plan_block(block: &[u8], prev: Option<u8>) -> BlockPlan {
    let freq = histogram(block);
    let plain = tables(&freq, 0);
    if flat_bytes(block) * RUN_SHARE >= block.len() {
        let mut freq = freq;
        let mut runs = Vec::new();
        let extra = find_runs(block, prev, &mut freq, &mut runs);
        if !runs.is_empty() {
            let with_runs = tables(&freq, extra);
            if with_runs.bits < plain.bits {
                return BlockPlan {
                    tables: with_runs,
                    runs: Some(runs),
                };
            }
        }
    }
    BlockPlan {
        tables: plain,
        runs: None,
    }
}

/// Size in bytes of `data` coded by [`deflate`], to within a few bytes (no closing marker).
pub fn size(data: &[u8]) -> usize {
    let mut total = 0u64;
    let mut prev = None;
    for block in data.chunks(BLOCK) {
        let plan = plan_block(block, prev);
        total += plan.tables.bits.min(8 * (block.len() as u64 + 5));
        prev = block.last().copied();
    }
    total.div_ceil(8) as usize
}

fn emit_stored(block: &[u8], last: bool, w: &mut Bits) {
    let mut chunks = block.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        w.put((last && chunks.peek().is_none()) as u32, 3); // BFINAL, BTYPE 00
        w.align();
        w.out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        w.out
            .extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        w.out.extend_from_slice(chunk);
    }
}

/// Codes `literals` with the block's table, two symbols per accumulator update.
#[inline(always)]
fn emit_literals(w: &mut Bits, literals: &[u8], table: &[(u32, u32)]) {
    let mut pairs = literals.chunks_exact(2);
    for p in &mut pairs {
        let (c0, l0) = table[p[0] as usize];
        let (c1, l1) = table[p[1] as usize];
        // Two codes of at most 15 bits each always fit the 64-bit accumulator after the
        // pending (fewer than 32) bits.
        w.acc |= (c0 as u64) << w.n;
        w.n += l0;
        w.acc |= (c1 as u64) << w.n;
        w.n += l1;
        if w.n >= 32 {
            w.out.extend_from_slice(&(w.acc as u32).to_le_bytes());
            w.acc >>= 32;
            w.n -= 32;
        }
    }
    for &v in pairs.remainder() {
        let (c, l) = table[v as usize];
        w.put(c, l);
    }
}

/// Appends a raw deflate stream for `data` to `out`; `before` is the byte that precedes `data`
/// in the full stream (runs may continue it). With `last`, the final block carries the
/// final-block flag; otherwise the stream is closed with an empty stored block (a sync flush)
/// so that another stream can follow byte-aligned.
pub fn deflate(data: &[u8], before: Option<u8>, last: bool, out: &mut Vec<u8>) {
    out.reserve(data.len() / 2 + 1024);
    let mut w = Bits { out, acc: 0, n: 0 };
    let blocks = data.chunks(BLOCK).count();
    let mut prev = before;
    for (index, block) in data.chunks(BLOCK).enumerate() {
        let is_final = last && index + 1 == blocks;
        let plan = plan_block(block, prev);
        prev = block.last().copied();
        let t = &plan.tables;
        if t.bits.div_ceil(8) as usize >= block.len() + 5 {
            emit_stored(block, is_final, &mut w);
            continue;
        }
        w.put(is_final as u32 | (2 << 1), 3);
        w.put(t.nlit as u32 - 257, 5); // HLIT
        w.put(1, 5); // HDIST: 2 codes
        w.put(t.hclen as u32 - 4, 4);
        for &o in &CL_ORDER[..t.hclen] {
            w.put(t.cl_lens[o] as u32, 3);
        }
        for &(s, extra) in &t.seq {
            let (code, len) = t.cl_codes[s as usize];
            w.put(code as u32, len as u32);
            match s {
                16 => w.put(extra as u32, 2),
                17 => w.put(extra as u32, 3),
                18 => w.put(extra as u32, 7),
                _ => {}
            }
        }
        let table: Vec<(u32, u32)> = t.codes.iter().map(|&(c, l)| (c as u32, l as u32)).collect();
        if let Some(runs) = &plan.runs {
            // Per run length: the length code, its extra bits, then distance code 0 (one bit,
            // value 0), as one bit string.
            let mut run_bits = [(0u32, 0u32); 256];
            for (i, run) in run_bits.iter_mut().enumerate() {
                let idx = LEN_INDEX[i] as usize;
                let (c, l) = table[257 + idx];
                let extra = (i + 3 - LEN_BASE[idx] as usize) as u32;
                *run = (c | (extra << l), l + LEN_EXTRA[idx] as u32 + 1);
            }
            let mut at = 0usize;
            for &(pos, len) in runs {
                let pos = pos as usize;
                emit_literals(&mut w, &block[at..pos], &table);
                let (c, l) = run_bits[len as usize - 3];
                w.put(c, l);
                at = pos + len as usize;
            }
            emit_literals(&mut w, &block[at..], &table);
        } else {
            emit_literals(&mut w, block, &table);
        }
        let (c, l) = table[256];
        w.put(c, l);
    }
    if data.is_empty() {
        // An empty stream still needs a block.
        w.put(last as u32 | (1 << 1), 3);
        w.put(0, 7); // fixed-Huffman end of block
    }
    if !last {
        w.put(0, 3); // empty stored block, as a sync flush
        w.align();
        w.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
    } else {
        w.align();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn inflate(raw: &[u8]) -> Vec<u8> {
        let mut back = Vec::new();
        flate2::read::DeflateDecoder::new(raw)
            .read_to_end(&mut back)
            .expect("valid deflate");
        back
    }

    fn data(n: usize, kind: u8) -> Vec<u8> {
        let mut state = 99u32;
        (0..n)
            .map(|i| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                match kind {
                    0 => (state >> 24) as u8,
                    1 => 7,
                    2 => ((state >> 24) % 3) as u8,
                    3 => (i % 251) as u8,
                    // Long runs of a few values.
                    5 => ((i / 700) % 4) as u8,
                    // Geometric-ish: forces deep trees.
                    _ => ((state >> 24).leading_zeros() as u8)
                        .wrapping_mul(17)
                        .wrapping_add((state >> 20 & 1) as u8),
                }
            })
            .collect()
    }

    #[test]
    fn round_trips() {
        for kind in 0..6 {
            for n in [0usize, 1, 2, 3, 100, 65535, 65536, 131_073, 300_001] {
                let d = data(n, kind);
                for last in [true, false] {
                    let mut out = Vec::new();
                    deflate(&d, None, last, &mut out);
                    if !last {
                        // A trailing final empty block makes the stream complete.
                        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
                    }
                    assert_eq!(inflate(&out), d, "kind={kind} n={n} last={last}");
                }
            }
        }
    }

    #[test]
    fn length_limit() {
        // Fibonacci frequencies give a maximally skewed tree (depth far beyond 15).
        let mut d = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        for sym in 0..28u8 {
            d.extend(std::iter::repeat_n(sym, a));
            (a, b) = (b, a + b);
        }
        let mut out = Vec::new();
        deflate(&d, None, true, &mut out);
        assert_eq!(inflate(&out), d);
    }

    #[test]
    fn concatenated_streams() {
        let (a, b) = (data(100_000, 2), data(70_000, 0));
        let mut out = Vec::new();
        deflate(&a, None, false, &mut out);
        deflate(&b, a.last().copied(), true, &mut out);
        let mut joined = a.clone();
        joined.extend_from_slice(&b);
        assert_eq!(inflate(&out), joined);
    }

    #[test]
    fn size_is_exact() {
        for kind in 0..6 {
            let d = data(150_000, kind);
            let mut out = Vec::new();
            deflate(&d, None, true, &mut out);
            // size() ignores the byte alignment of the whole stream: within a byte per block.
            let s = size(&d);
            assert!(out.len().abs_diff(s) <= 12, "kind={kind} {} {s}", out.len());
        }
    }
}
