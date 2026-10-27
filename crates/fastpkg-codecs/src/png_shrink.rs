//! Decode-time area-average shrink for PNG rows.
//!
//! Source pixels are averaged into `out` cells with exact fractional edge weights (a box
//! filter whose width is the real factor `src / out`), so the resampler that follows sees an
//! exactly aligned, exactly scaled image whatever the source size is. Rows arrive one at a
//! time, either top to bottom ([`RowShrink`], two accumulator rows resident) or in Adam7 pass
//! order ([`InterlacedShrink`], one accumulator cell per output pixel resident). Both use the
//! same per-axis weights, so the two give the same result up to float rounding.
//!
//! With alpha the colour channels are averaged premultiplied (as the resizer does) and
//! un-premultiplied again on output.

/// How the pixels of one axis fall into output cells: pixel `i` adds `w0[i]` to cell `cell[i]`
/// and `w1[i]` (zero when the pixel lies inside one cell) to the next one. Every cell's weights
/// sum to one.
pub struct Axis {
    pub out: usize,
    cell: Vec<u32>,
    w0: Vec<f32>,
    w1: Vec<f32>,
}

impl Axis {
    pub fn new(src: usize, out: usize) -> Self {
        assert!(out >= 1 && out <= src);
        let span = src as f64 / out as f64;
        let mut cell = Vec::with_capacity(src);
        let mut w0 = Vec::with_capacity(src);
        let mut w1 = Vec::with_capacity(src);
        for i in 0..src {
            // Exact integer cell lookup; the boundary itself is computed in f64.
            let k = ((i as u128 * out as u128) / src as u128) as usize;
            let end = ((k + 1) as f64 * span).min(src as f64);
            let first = (end.min((i + 1) as f64) - i as f64).max(0.0);
            let rest = (i + 1) as f64 - end;
            cell.push(k as u32);
            if rest > 1e-12 && k + 1 < out {
                w0.push((first / span) as f32);
                w1.push((rest / span) as f32);
            } else {
                w0.push((1.0 / span) as f32);
                w1.push(0.0);
            }
        }
        Self { out, cell, w0, w1 }
    }

    /// Cell and weights of source pixel `i`.
    #[inline(always)]
    pub fn at(&self, i: usize) -> (usize, f32, f32) {
        (self.cell[i] as usize, self.w0[i], self.w1[i])
    }
}

/// A sample type of the RGBA rows being shrunk.
pub trait Sample: Copy + Default {
    const MAX: f32;
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Sample for u8 {
    const MAX: f32 = 255.0;
    #[inline(always)]
    fn to_f32(self) -> f32 {
        self as f32
    }
    #[inline(always)]
    fn from_f32(v: f32) -> Self {
        (v + 0.5).clamp(0.0, 255.0) as u8
    }
}

impl Sample for u16 {
    const MAX: f32 = 65_535.0;
    #[inline(always)]
    fn to_f32(self) -> f32 {
        self as f32
    }
    #[inline(always)]
    fn from_f32(v: f32) -> Self {
        (v + 0.5).clamp(0.0, 65_535.0) as u16
    }
}

/// The part shared by both shrinkers: horizontal reduction of one row of RGBA pixels into
/// `out_w` cells, and conversion of a finished accumulator row back to samples.
pub struct Core {
    pub xs: Axis,
    pub ys: Axis,
    has_alpha: bool,
    hrow: Vec<f32>,
}

impl Core {
    pub fn new(src: (usize, usize), out: (usize, usize), has_alpha: bool) -> Self {
        Self {
            xs: Axis::new(src.0, out.0),
            ys: Axis::new(src.1, out.1),
            has_alpha,
            hrow: vec![0.0; out.0 * 4],
        }
    }

    /// Reduces a row of RGBA pixels, the `n`-th of which sits at source column `x0 + n * dx`,
    /// into `self.hrow`.
    fn reduce<T: Sample>(&mut self, row: &[T], x0: usize, dx: usize) {
        self.hrow.fill(0.0);
        let hrow = &mut self.hrow;
        let xs = &self.xs;
        for (n, px) in row.chunks_exact(4).enumerate() {
            let (cell, w0, w1) = xs.at(x0 + n * dx);
            let (mut r, mut g, mut b) = (px[0].to_f32(), px[1].to_f32(), px[2].to_f32());
            let mut a = 0.0;
            if self.has_alpha {
                a = px[3].to_f32();
                r *= a;
                g *= a;
                b *= a;
            }
            let v = [r, g, b, a];
            let d = &mut hrow[cell * 4..cell * 4 + 4];
            for k in 0..4 {
                d[k] += v[k] * w0;
            }
            if w1 != 0.0 {
                let d = &mut hrow[cell * 4 + 4..cell * 4 + 8];
                for k in 0..4 {
                    d[k] += v[k] * w1;
                }
            }
        }
    }

    /// Adds the reduced row to the accumulator rows of the cell row source line `y` falls into.
    fn add_to(&self, y: usize, first: &mut [f32], second: Option<&mut [f32]>) -> (usize, bool) {
        let (cell, w0, w1) = self.ys.at(y);
        for (d, s) in first.iter_mut().zip(&self.hrow) {
            *d += s * w0;
        }
        if w1 != 0.0 {
            if let Some(second) = second {
                for (d, s) in second.iter_mut().zip(&self.hrow) {
                    *d += s * w1;
                }
            }
        }
        (cell, w1 != 0.0)
    }

    fn finish<T: Sample>(&self, acc: &[f32], out: &mut [T]) {
        for (a, px) in acc.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
            if self.has_alpha {
                let alpha = a[3];
                if alpha > 0.0 {
                    let inv = 1.0 / alpha;
                    px[0] = T::from_f32(a[0] * inv);
                    px[1] = T::from_f32(a[1] * inv);
                    px[2] = T::from_f32(a[2] * inv);
                } else {
                    px[..3].fill(T::default());
                }
                px[3] = T::from_f32(alpha);
            } else {
                px[0] = T::from_f32(a[0]);
                px[1] = T::from_f32(a[1]);
                px[2] = T::from_f32(a[2]);
                px[3] = T::from_f32(T::MAX);
            }
        }
    }
}

/// Top-to-bottom shrink: only the cell rows the current source row touches are resident.
pub struct RowShrink {
    core: Core,
    /// Accumulator rows of cell rows `cell` and `cell + 1`.
    rows: [Vec<f32>; 2],
    cell: usize,
}

impl RowShrink {
    pub fn new(src: (usize, usize), out: (usize, usize), has_alpha: bool) -> Self {
        Self {
            core: Core::new(src, out, has_alpha),
            rows: [vec![0.0; out.0 * 4], vec![0.0; out.0 * 4]],
            cell: 0,
        }
    }

    /// Feeds source row `y` (rows must come in order). Cell rows that are complete are written
    /// through `emit(cell_row, samples)`.
    pub fn add_row<T: Sample>(&mut self, y: usize, row: &[T], mut emit: impl FnMut(usize, &[T])) {
        let (cell, _, _) = self.core.ys.at(y);
        while self.cell < cell {
            self.flush(&mut emit);
        }
        self.core.reduce(row, 0, 1);
        let [first, second] = &mut self.rows;
        self.core.add_to(y, first, Some(second));
    }

    /// Writes cell row `self.cell` and slides the window down by one.
    fn flush<T: Sample>(&mut self, emit: &mut impl FnMut(usize, &[T])) {
        let mut out = vec![T::default(); self.rows[0].len()];
        self.core.finish(&self.rows[0], &mut out);
        emit(self.cell, &out);
        self.rows.swap(0, 1);
        self.rows[1].fill(0.0);
        self.cell += 1;
    }

    /// Emits the remaining cell rows once the last source row has been added.
    pub fn finish<T: Sample>(&mut self, mut emit: impl FnMut(usize, &[T])) {
        while self.cell < self.core.ys.out {
            self.flush(&mut emit);
        }
    }
}

/// Shrink for rows that arrive out of order (Adam7 passes): every output cell is accumulated
/// in place, so the accumulator is four `f32` per output pixel.
pub struct InterlacedShrink {
    core: Core,
    acc: Vec<f32>,
}

impl InterlacedShrink {
    pub fn new(src: (usize, usize), out: (usize, usize), has_alpha: bool) -> Self {
        Self {
            core: Core::new(src, out, has_alpha),
            acc: vec![0.0; out.0 * out.1 * 4],
        }
    }

    /// Feeds a pass row: source row `y`, the `n`-th pixel of `row` is at column `x0 + n * dx`.
    pub fn add_row<T: Sample>(&mut self, y: usize, row: &[T], x0: usize, dx: usize) {
        self.core.reduce(row, x0, dx);
        let stride = self.core.xs.out * 4;
        let (cell, _, w1) = self.core.ys.at(y);
        let from = cell * stride;
        if w1 != 0.0 {
            let (head, tail) = self.acc[from..].split_at_mut(stride);
            self.core.add_to(y, head, Some(&mut tail[..stride]));
        } else {
            self.core.add_to(y, &mut self.acc[from..from + stride], None);
        }
    }

    /// The shrunk image as RGBA samples.
    pub fn finish<T: Sample>(self) -> Vec<T> {
        let mut out = vec![T::default(); self.acc.len()];
        let stride = self.core.xs.out * 4;
        for (acc, out) in self.acc.chunks_exact(stride).zip(out.chunks_exact_mut(stride)) {
            self.core.finish(acc, out);
        }
        out
    }
}

/// Headrooms to try, best accuracy first: the resampler keeps this many times the target size
/// after the shrink. Against a full-resolution float Lanczos reference, 4 comes within about a
/// dB of resampling the whole image and beats sharp (libvips shrinks by an integer factor to
/// 2x and is several dB off on thin lines); 2 is the floor, where it ties with sharp.
const HEADROOMS: [f64; 4] = [4.0, 3.0, 2.5, 2.0];

/// Smallest source-to-intermediate ratio worth shrinking an axis for.
const MIN_RATIO: f64 = 1.25;

/// Size of the intermediate along one axis: `headroom` times the target, or the source itself
/// when shrinking would gain less than [`MIN_RATIO`].
fn axis_size(src: u32, target: u32, headroom: f64) -> u32 {
    let wanted = (target.max(1) as f64 * headroom).ceil();
    if wanted * MIN_RATIO > src as f64 {
        src
    } else {
        wanted as u32
    }
}

/// Size of the image the rows are shrunk to, or `None` to decode at full size. Takes the
/// largest headroom whose intermediate (`cell_bytes` per pixel while it is built and handed
/// on) fits `budget`, and the smallest one when none does.
pub fn plan(src: (u32, u32), target: (u32, u32), cell_bytes: u64, budget: u64) -> Option<(u32, u32)> {
    let size = |headroom| (axis_size(src.0, target.0, headroom), axis_size(src.1, target.1, headroom));
    let (w, h) = HEADROOMS
        .iter()
        .map(|&headroom| size(headroom))
        .find(|&(w, h)| w as u64 * h as u64 * cell_bytes <= budget)
        .unwrap_or_else(|| size(HEADROOMS[HEADROOMS.len() - 1]));
    ((w, h) != src).then_some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell_sums(src: usize, out: usize) -> Vec<f64> {
        let axis = Axis::new(src, out);
        let mut sums = vec![0.0f64; out];
        for i in 0..src {
            let (c, w0, w1) = axis.at(i);
            sums[c] += w0 as f64;
            if w1 != 0.0 {
                sums[c + 1] += w1 as f64;
            }
        }
        sums
    }

    #[test]
    fn cells_have_unit_weight() {
        for (src, out) in [(100, 10), (101, 10), (1000, 333), (7, 3), (16384, 1639), (50, 50)] {
            for (k, sum) in cell_sums(src, out).iter().enumerate() {
                assert!((sum - 1.0).abs() < 1e-5, "{src}->{out} cell {k}: {sum}");
            }
        }
    }

    #[test]
    fn integer_factor_is_a_plain_box() {
        let axis = Axis::new(12, 3);
        for i in 0..12 {
            let (c, w0, w1) = axis.at(i);
            assert_eq!((c, w1), (i / 4, 0.0));
            assert!((w0 - 0.25).abs() < 1e-7);
        }
    }

    fn picture(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 4)
            .map(|i| ((i * 2654435761usize) >> 7) as u8)
            .collect()
    }

    #[test]
    fn streaming_and_interlaced_agree() {
        let (w, h) = (53, 47);
        let (ow, oh) = (11, 9);
        let px = picture(w, h);
        for alpha in [false, true] {
            let mut rows = RowShrink::new((w, h), (ow, oh), alpha);
            let mut a = vec![0u8; ow * oh * 4];
            for y in 0..h {
                rows.add_row(y, &px[y * w * 4..(y + 1) * w * 4], |cy, out: &[u8]| {
                    a[cy * ow * 4..(cy + 1) * ow * 4].copy_from_slice(out)
                });
            }
            rows.finish(|cy, out: &[u8]| a[cy * ow * 4..(cy + 1) * ow * 4].copy_from_slice(out));
            // Adam7 order.
            let mut inter = InterlacedShrink::new((w, h), (ow, oh), alpha);
            for (x0, y0, dx, dy) in [
                (0, 0, 8, 8),
                (4, 0, 8, 8),
                (0, 4, 4, 8),
                (2, 0, 4, 4),
                (0, 2, 2, 4),
                (1, 0, 2, 2),
                (0, 1, 1, 2),
            ] {
                for y in (y0..h).step_by(dy) {
                    let row: Vec<u8> = (x0..w)
                        .step_by(dx)
                        .flat_map(|x| px[(y * w + x) * 4..(y * w + x) * 4 + 4].to_vec())
                        .collect();
                    inter.add_row(y, &row, x0, dx);
                }
            }
            let b: Vec<u8> = inter.finish();
            let worst = a.iter().zip(&b).map(|(p, q)| p.abs_diff(*q)).max();
            assert!(worst <= Some(1), "alpha {alpha}: {worst:?}");
        }
    }

    #[test]
    fn flat_colour_stays_flat() {
        let (w, h) = (97, 61);
        let px: Vec<u16> = std::iter::repeat([40_000u16, 1, 65_535, 65_535])
            .take(w * h)
            .flatten()
            .collect();
        let mut rows = RowShrink::new((w, h), (10, 6), false);
        let mut out = vec![0u16; 10 * 6 * 4];
        for y in 0..h {
            rows.add_row(y, &px[y * w * 4..(y + 1) * w * 4], |cy, o: &[u16]| {
                out[cy * 40..(cy + 1) * 40].copy_from_slice(o)
            });
        }
        rows.finish(|cy, o: &[u16]| out[cy * 40..(cy + 1) * 40].copy_from_slice(o));
        assert!(out.chunks(4).all(|p| p == [40_000, 1, 65_535, 65_535]));
    }
}
