//! Resizing from rows: the integer box shrink and the kernel resample of `resize_raw_opts`, done
//! band by band while the source rows arrive (from a decoder, or from a frame that is already in
//! memory), so only a window of source rows plus the destination is resident instead of the
//! source, its box-shrunk and padded copies, and the destination.
//!
//! The box shrink and the edge padding are applied to rows as they come. A band of destination
//! rows is resampled as soon as the source rows it depends on exist, with a crop box that places
//! the window exactly where the whole image would have put it (the construction `resize_banded`
//! uses), so the filter weights are those of the one-shot resize and the two agree to float
//! rounding.

use bytemuck::Pod;
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{PixelType, ResizeOptions, Resizer};
use fastpkg_core::image::narrow_sample;
use fastpkg_core::options::{Kernel, Operation};
use fastpkg_core::{FastpkgError, Image, Result, RowSink, RowSinkSpec};
use rayon::prelude::*;

/// A sample type of RGBA pixels: 8-bit, or 16-bit. `Acc` is wide enough to add the products of
/// a block of samples and alphas.
pub(crate) trait Sample:
    Copy + Default + Send + Sync + Pod + Into<u64> + TryFrom<u64> + 'static
{
    type Acc: Copy
        + Default
        + Send
        + Sync
        + Into<u64>
        + std::ops::AddAssign
        + std::ops::Mul<Output = Self::Acc>;
    const PIXEL: PixelType;
    /// Block areas below this are divided by reciprocal multiplication (see `divide`).
    const FAST_AREA: u64;
    fn wide(self) -> Self::Acc;
}

impl Sample for u8 {
    type Acc = u32;
    const PIXEL: PixelType = PixelType::U8x4;
    const FAST_AREA: u64 = 300_000;
    #[inline(always)]
    fn wide(self) -> u32 {
        self as u32
    }
}

impl Sample for u16 {
    type Acc = u64;
    const PIXEL: PixelType = PixelType::U16x4;
    const FAST_AREA: u64 = 250;
    #[inline(always)]
    fn wide(self) -> u64 {
        self as u64
    }
}

/// `n / d` by multiplying with the reciprocal `m = ceil(2^64 / d)` (`d >= 2`): exact while
/// `n * d <= 2^64`, which the callers guarantee through `Sample::FAST_AREA`.
#[inline(always)]
fn divide(n: u64, m: u64) -> u64 {
    ((n as u128 * m as u128) >> 64) as u64
}

#[inline(always)]
fn reciprocal(d: u64) -> u64 {
    (u64::MAX / d).wrapping_add(1)
}

/// One row of the box shrink: `lines` (the `ky` source rows of a block row, each `width` pixels
/// of four samples, repeated at the bottom edge by the caller) averaged in blocks of `kx`
/// pixels, the last block completed by repeating the edge pixel. With `weighted` the colour is
/// weighted by alpha, so a transparent pixel does not dilute. `acc` is scratch.
pub(crate) fn box_row<T: Sample>(
    lines: &[&[T]],
    width: usize,
    kx: usize,
    weighted: bool,
    acc: &mut Vec<T::Acc>,
    out: &mut [T],
) {
    let area = (kx * lines.len()) as u64;
    acc.clear();
    acc.resize(width * 4, T::Acc::default());
    // Vertical sums first, over the whole row: plain element-wise adds that vectorise.
    for line in lines {
        let line = &line[..width * 4];
        if weighted {
            for (a, px) in acc.chunks_exact_mut(4).zip(line.chunks_exact(4)) {
                let alpha = px[3].wide();
                a[0] += px[0].wide() * alpha;
                a[1] += px[1].wide() * alpha;
                a[2] += px[2].wide() * alpha;
                a[3] += alpha;
            }
        } else {
            for (a, s) in acc.iter_mut().zip(line) {
                *a += s.wide();
            }
        }
    }
    let narrow = |v: u64| T::try_from(v).unwrap_or_default();
    // Block area, its reciprocal, and whether reciprocal multiplication is exact for it (u8
    // sums never get near the bound; 16-bit ones weighted by alpha only for small blocks).
    let fast = area >= 2 && area < T::FAST_AREA;
    let area_m = if fast { reciprocal(area) } else { 0 };
    let by_area = |n: u64| if fast { divide(n, area_m) } else { n / area };
    for (ox, px) in out.chunks_exact_mut(4).enumerate() {
        let x0 = ox * kx;
        let mut sum = [0u64; 4];
        if x0 + kx <= width {
            for cell in acc[x0 * 4..(x0 + kx) * 4].chunks_exact(4) {
                for c in 0..4 {
                    sum[c] += cell[c].into();
                }
            }
        } else {
            for dx in 0..kx {
                let x = (x0 + dx).min(width - 1);
                for c in 0..4 {
                    sum[c] += acc[x * 4 + c].into();
                }
            }
        }
        if weighted {
            // A block that is entirely transparent has no colour to average.
            let weight = sum[3];
            if weight == 0 {
                px[..3].fill(T::default());
            } else if fast && weight >= 2 {
                let m = reciprocal(weight);
                for c in 0..3 {
                    px[c] = narrow(divide(sum[c] + weight / 2, m));
                }
            } else {
                for c in 0..3 {
                    px[c] = narrow((sum[c] + weight / 2) / weight);
                }
            }
        } else {
            for c in 0..3 {
                px[c] = narrow(by_area(sum[c] + area / 2));
            }
        }
        px[3] = narrow(by_area(sum[3] + area / 2));
    }
}

/// Mean of every `kx` x `ky` block of `data` (`width` x `height` pixels of four samples), the
/// way `vips_shrink` does it: the output is rounded up and a partial last block is completed by
/// repeating the edge. Channels of an image with alpha are weighted by it (the average of the
/// premultiplied pixels, un-premultiplied again).
pub(crate) fn box_shrink<T: Sample>(
    data: &[T],
    (width, height): (usize, usize),
    (kx, ky): (usize, usize),
    weighted: bool,
) -> (Vec<T>, usize, usize) {
    let (out_w, out_h) = (width.div_ceil(kx), height.div_ceil(ky));
    let mut out = vec![data[0]; out_w * out_h * 4];
    out.par_chunks_mut(out_w * 4)
        .enumerate()
        .for_each_init(Vec::new, |acc, (oy, row)| {
            let lines: Vec<&[T]> = (0..ky)
                .map(|dy| &data[(oy * ky + dy).min(height - 1) * width * 4..][..width * 4])
                .collect();
            box_row(&lines, width, kx, weighted, acc, row);
        });
    (out, out_w, out_h)
}

/// What a resize does to the rows it is fed, as decided by `resize_plan`.
#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    /// Size of the rows that are pushed.
    pub fed: (usize, usize),
    pub out: (usize, usize),
    pub box_factor: (usize, usize),
    /// Left, top, right and bottom padding of the box-shrunk image.
    pub pad: [usize; 4],
    /// Left, top, width and height of the sampled region of the padded image.
    pub crop: [f64; 4],
    pub canvas: (usize, usize),
    pub weighted: bool,
}

/// Bytes of source rows kept for one band, margin included.
const WINDOW_BYTES: usize = 8 << 20;

/// Most bytes of un-shrunk rows buffered for the vertical box factor.
const BOX_BYTES: usize = 64 << 20;

impl Geometry {
    pub fn new(
        fed: (u32, u32),
        out: (u32, u32),
        plan: Option<&super::ResizePlan>,
        weighted: bool,
    ) -> Self {
        let (box_factor, pad, crop, canvas) = match plan {
            Some(plan) => (
                (plan.box_factor.0 as usize, plan.box_factor.1 as usize),
                plan.pad.map(|p| p as usize),
                plan.crop,
                (plan.canvas.0 as usize, plan.canvas.1 as usize),
            ),
            None => (
                (1, 1),
                [0; 4],
                [0.0, 0.0, fed.0 as f64, fed.1 as f64],
                (fed.0 as usize, fed.1 as usize),
            ),
        };
        Self {
            fed: (fed.0 as usize, fed.1 as usize),
            out: (out.0 as usize, out.1 as usize),
            box_factor,
            pad,
            crop,
            canvas,
            weighted,
        }
    }

    /// Source rows per destination row.
    fn scale(&self) -> f64 {
        self.crop[3] / self.out.1.max(1) as f64
    }

    /// Whether the rows can be resampled as a stream: a vertical downscale (banding does not
    /// apply to anything else), and a box factor that does not need a huge buffer of rows.
    pub fn streamable<T: Sample>(&self) -> bool {
        self.scale() > 1.0
            && self.out.0 > 0
            && self.out.1 > 0
            && self.box_factor.1 * self.fed.0 * 4 * std::mem::size_of::<T>() <= BOX_BYTES
    }
}

/// Where the resized samples go.
pub(crate) enum Output<T> {
    /// The destination image, in `T` samples.
    Same(Vec<T>),
    /// 16-bit samples narrowed to 8 bits as each band is finished, for results that nothing
    /// else will need at 16 bits.
    Narrowed(Vec<u8>),
}

pub(crate) struct Engine<T: Sample> {
    g: Geometry,
    options: ResizeOptions,
    resizer: Resizer,
    scale: f64,
    margin: usize,
    band_rows: usize,
    next_y: usize,
    /// Rows of the vertical box block being collected.
    block: Vec<T>,
    block_rows: usize,
    acc: Vec<T::Acc>,
    shrunk: Vec<T>,
    shrunk_rows: usize,
    fed_rows: usize,
    /// Rows of the padded canvas from `win_lo` on (those the next bands still need).
    window: Vec<T>,
    win_lo: usize,
    emitted: usize,
    out: Output<T>,
    band_out: Vec<T>,
}

fn operation_error(error: impl std::fmt::Display) -> FastpkgError {
    FastpkgError::Operation(error.to_string())
}

impl<T: Sample> Engine<T> {
    pub fn new(g: Geometry, options: ResizeOptions, narrow: bool) -> Self {
        Self::with_window(g, options, narrow, WINDOW_BYTES)
    }

    /// [`Engine::new`] with a chosen budget for the rows kept per band (tests use tiny ones).
    pub fn with_window(g: Geometry, options: ResizeOptions, narrow: bool, window: usize) -> Self {
        let scale = g.scale();
        let margin = (5.0 * scale.max(1.0)).ceil() as usize + 2;
        let row_bytes = g.canvas.0 * 4 * std::mem::size_of::<T>();
        let rows = (window / row_bytes.max(1)).saturating_sub(2 * margin);
        let band_rows = ((rows as f64 / scale) as usize)
            .clamp(4, g.out.1.max(4))
            .min(g.out.1);
        let samples = g.out.0 * g.out.1 * 4;
        let out = if narrow && std::mem::size_of::<T>() == 2 {
            Output::Narrowed(vec![0; samples])
        } else {
            Output::Same(vec![T::default(); samples])
        };
        Self {
            block: Vec::with_capacity(g.box_factor.1 * g.fed.0 * 4),
            shrunk: vec![T::default(); g.fed.0.div_ceil(g.box_factor.0) * 4],
            g,
            options,
            resizer: Resizer::new(),
            scale,
            margin,
            band_rows,
            next_y: 0,
            block_rows: 0,
            acc: Vec::new(),
            shrunk_rows: 0,
            fed_rows: 0,
            window: Vec::new(),
            win_lo: 0,
            emitted: 0,
            out,
            band_out: Vec::new(),
        }
    }

    /// Feeds the next source row (`fed.0` pixels of four samples).
    pub fn push_row(&mut self, row: &[T]) -> Result<()> {
        if row.len() != self.g.fed.0 * 4 || self.fed_rows >= self.g.fed.1 {
            return Err(operation_error("resize: unexpected source row"));
        }
        self.fed_rows += 1;
        if self.g.box_factor == (1, 1) {
            self.append_shrunk(row);
        } else {
            self.block.extend_from_slice(row);
            self.block_rows += 1;
            if self.block_rows == self.g.box_factor.1 {
                self.flush_block();
            }
        }
        self.run_bands(false)
    }

    /// Feeds a row that is already box-shrunk (`ceil(fed.0 / box_factor.0)` pixels).
    fn push_shrunk(&mut self, row: &[T]) -> Result<()> {
        self.append_shrunk(row);
        self.run_bands(false)
    }

    /// Box-shrinks the collected block rows (a partial block at the bottom edge repeats its last
    /// row) into one shrunk row.
    fn flush_block(&mut self) {
        let width = self.g.fed.0 * 4;
        let ky = self.g.box_factor.1;
        let last = (self.block_rows - 1) * width;
        let lines: Vec<&[T]> = (0..ky)
            .map(|dy| {
                let at = (dy * width).min(last);
                &self.block[at..at + width]
            })
            .collect();
        let mut shrunk = std::mem::take(&mut self.shrunk);
        box_row(
            &lines,
            self.g.fed.0,
            self.g.box_factor.0,
            self.g.weighted,
            &mut self.acc,
            &mut shrunk,
        );
        self.block.clear();
        self.block_rows = 0;
        self.append_shrunk(&shrunk);
        self.shrunk = shrunk;
    }

    /// Appends a shrunk row to the canvas with its left and right padding; the first one also
    /// stands for the top padding.
    fn append_shrunk(&mut self, row: &[T]) {
        let [left, top, right, _] = self.g.pad;
        let reps = if self.shrunk_rows == 0 { top + 1 } else { 1 };
        let start = self.window.len();
        for _ in 0..left {
            self.window.extend_from_slice(&row[..4]);
        }
        self.window.extend_from_slice(row);
        for _ in 0..right {
            self.window.extend_from_slice(&row[row.len() - 4..]);
        }
        let len = self.window.len() - start;
        for _ in 1..reps {
            self.window.extend_from_within(start..start + len);
        }
        self.emitted += reps;
        self.shrunk_rows += 1;
    }

    /// Source rows of the padded canvas a band of destination rows `y0..y1` reads.
    fn span(&self, y0: usize, y1: usize) -> (usize, usize) {
        let top = self.g.crop[1];
        let lo = ((top + y0 as f64 * self.scale) - self.margin as f64)
            .floor()
            .max(0.0) as usize;
        let hi = (((top + y1 as f64 * self.scale) + self.margin as f64).ceil() as usize)
            .min(self.g.canvas.1);
        (lo, hi)
    }

    /// Resamples every band whose source rows have all arrived (all of them, at the end).
    fn run_bands(&mut self, last: bool) -> Result<()> {
        let dh = self.g.out.1;
        while self.next_y < dh {
            let (y0, y1) = (self.next_y, (self.next_y + self.band_rows).min(dh));
            let (lo, hi) = self.span(y0, y1);
            if self.emitted < hi && !last {
                break;
            }
            self.resample_band(y0, y1, lo, hi)?;
            self.next_y = y1;
            if y1 < dh {
                // Rows the following bands no longer reach can go.
                let (next_lo, _) = self.span(y1, (y1 + 1).min(dh));
                self.drop_rows_before(next_lo);
            }
        }
        Ok(())
    }

    fn drop_rows_before(&mut self, row: usize) {
        let row_len = self.g.canvas.0 * 4;
        let held = self.window.len() / row_len;
        let drop = row.saturating_sub(self.win_lo).min(held);
        if drop > 0 {
            self.window.drain(..drop * row_len);
            self.win_lo += drop;
        }
    }

    fn resample_band(&mut self, y0: usize, y1: usize, lo: usize, hi: usize) -> Result<()> {
        let [crop_left, crop_top, crop_width, _] = self.g.crop;
        let row_len = self.g.canvas.0 * 4;
        let (dw, rows) = (self.g.out.0, y1 - y0);
        let held = self.window.len() / row_len;
        if lo < self.win_lo || hi > self.win_lo + held {
            return Err(operation_error("resize: source rows missing"));
        }
        let samples = &self.window[(lo - self.win_lo) * row_len..(hi - self.win_lo) * row_len];
        let band_src = ImageRef::new(
            self.g.canvas.0 as u32,
            (hi - lo) as u32,
            bytemuck::cast_slice(samples),
            T::PIXEL,
        )
        .map_err(operation_error)?;
        // Rounding must not push the box past the slice (it would be rejected).
        let top = crop_top + y0 as f64 * self.scale - lo as f64;
        let height = (rows as f64 * self.scale).min((hi - lo) as f64 - top);
        let options = self.options.crop(crop_left, top, crop_width, height);
        let range = y0 * dw * 4..y1 * dw * 4;
        match &mut self.out {
            Output::Same(out) => {
                let mut band = FirImage::from_slice_u8(
                    dw as u32,
                    rows as u32,
                    bytemuck::cast_slice_mut(&mut out[range]),
                    T::PIXEL,
                )
                .map_err(operation_error)?;
                self.resizer
                    .resize(&band_src, &mut band, Some(&options))
                    .map_err(operation_error)?;
            }
            Output::Narrowed(out) => {
                self.band_out.resize(rows * dw * 4, T::default());
                let mut band = FirImage::from_slice_u8(
                    dw as u32,
                    rows as u32,
                    bytemuck::cast_slice_mut(&mut self.band_out),
                    T::PIXEL,
                )
                .map_err(operation_error)?;
                self.resizer
                    .resize(&band_src, &mut band, Some(&options))
                    .map_err(operation_error)?;
                for (d, s) in out[range].iter_mut().zip(&self.band_out) {
                    // Only 16-bit samples are ever narrowed.
                    let wide: u64 = (*s).into();
                    *d = narrow_sample(wide as u16);
                }
            }
        }
        Ok(())
    }

    /// Completes the last block and the bottom padding, resamples what is left and returns the
    /// destination samples.
    pub fn finish(mut self) -> Result<Output<T>> {
        if self.fed_rows != self.g.fed.1 {
            return Err(operation_error("resize: the source ended early"));
        }
        if self.block_rows > 0 {
            self.flush_block();
        }
        let row_len = self.g.canvas.0 * 4;
        let held = self.window.len() / row_len;
        if held == 0 {
            return Err(operation_error("resize: no source rows"));
        }
        for _ in 0..self.g.pad[3] {
            let from = self.window.len() - row_len;
            self.window.extend_from_within(from..);
            self.emitted += 1;
        }
        self.run_bands(true)?;
        Ok(self.out)
    }
}

/// Resamples a frame that is already in memory with the engine (no box-shrunk or padded copy of
/// it, no whole-frame intermediate). All the source rows exist, so the box shrink runs in
/// parallel over chunks of shrunk rows.
pub(crate) fn resize_frame<T: Sample>(
    data: &[T],
    g: Geometry,
    options: ResizeOptions,
) -> Result<Vec<T>> {
    let (width, height) = g.fed;
    let (kx, ky) = g.box_factor;
    let weighted = g.weighted;
    let mut engine = Engine::<T>::new(g, options, false);
    engine.fed_rows = height;
    if (kx, ky) == (1, 1) {
        for row in data.chunks_exact(width * 4) {
            engine.push_shrunk(row)?;
        }
    } else {
        let (out_w, out_h) = (width.div_ceil(kx), height.div_ceil(ky));
        let chunk = 32;
        let mut buf = vec![T::default(); chunk * out_w * 4];
        for first in (0..out_h).step_by(chunk) {
            let rows = chunk.min(out_h - first);
            buf[..rows * out_w * 4]
                .par_chunks_mut(out_w * 4)
                .enumerate()
                .for_each_init(Vec::new, |acc, (i, row)| {
                    let lines: Vec<&[T]> = (0..ky)
                        .map(|dy| {
                            let y = ((first + i) * ky + dy).min(height - 1);
                            &data[y * width * 4..][..width * 4]
                        })
                        .collect();
                    box_row(&lines, width, kx, weighted, acc, row);
                });
            for row in buf[..rows * out_w * 4].chunks_exact(out_w * 4) {
                engine.push_shrunk(row)?;
            }
        }
    }
    match engine.finish()? {
        Output::Same(out) => Ok(out),
        Output::Narrowed(_) => Err(operation_error("resize: unexpected narrowed output")),
    }
}

/// Frames at least this big (bytes of samples) are resized with the engine; below, the one-shot
/// resize is quicker and the extra copies are cheap. `FASTPKG_RESIZE_STREAM_MIN_BYTES` overrides
/// it (tests).
pub(crate) fn stream_min_bytes() -> usize {
    static MIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MIN.get_or_init(|| {
        std::env::var("FASTPKG_RESIZE_STREAM_MIN_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16 << 20)
    })
}

enum Kind {
    Narrow(Engine<u8>),
    Wide(Engine<u16>),
}

/// The decoder-facing end: resamples the rows a decoder pushes and returns the resized image.
struct ResizeSink {
    kind: Kind,
    out: (u32, u32),
    has_alpha: bool,
}

impl RowSink for ResizeSink {
    fn push_u8(&mut self, row: &[u8]) -> Result<()> {
        match &mut self.kind {
            Kind::Narrow(engine) => engine.push_row(row),
            Kind::Wide(_) => Err(operation_error("resize: 8-bit row for a 16-bit image")),
        }
    }

    fn push_u16(&mut self, row: &[u16]) -> Result<()> {
        match &mut self.kind {
            Kind::Wide(engine) => engine.push_row(row),
            Kind::Narrow(_) => Err(operation_error("resize: 16-bit row for an 8-bit image")),
        }
    }

    fn finish(self: Box<Self>) -> Result<Image> {
        let Self {
            kind,
            out: (width, height),
            has_alpha,
        } = *self;
        let mut image = match kind {
            Kind::Narrow(engine) => match engine.finish()? {
                Output::Same(data) => Image::from_rgba(width, height, data)?,
                Output::Narrowed(_) => unreachable!("8-bit samples are not narrowed"),
            },
            Kind::Wide(engine) => match engine.finish()? {
                Output::Same(data) => Image::from_rgba16(width, height, data)?,
                Output::Narrowed(data) => Image::from_rgba(width, height, data)?,
            },
        };
        image.has_alpha = has_alpha || image.has_transparency();
        image.presized = true;
        Ok(image)
    }
}

/// A sink that resamples the rows of `spec` for the resize `op`, to be fed by a decoder, or
/// `None` when this resize is not one that can be done from a stream of rows (not a plain
/// downscale, nearest-neighbour kernel, ...) and the image should be decoded and resized as
/// usual. `narrow` lets 16-bit samples be narrowed to 8 bits in the result: pass it when
/// nothing after the resize needs the wide samples.
pub fn resize_sink(op: &Operation, spec: &RowSinkSpec, narrow: bool) -> Option<Box<dyn RowSink>> {
    let Operation::Resize {
        width,
        height,
        fit,
        kernel,
        without_enlargement,
        without_reduction,
        ..
    } = op
    else {
        return None;
    };
    if (width.is_none() && height.is_none()) || *kernel == Kernel::Nearest {
        return None;
    }
    let (sw, sh) = spec.source;
    let (rw, rh) = fastpkg_core::pipeline::compute_target(
        sw,
        sh,
        *width,
        *height,
        *fit,
        *without_enlargement,
        *without_reduction,
    );
    if (rw, rh) == spec.fed {
        return None;
    }
    // A shrunk source is resampled by the ratio of the rounded sizes; only an unshrunk one
    // can follow the shrink factors themselves (see `resize`).
    let scales = (spec.fed == spec.source).then(|| {
        fastpkg_core::pipeline::resolve_shrink(
            sw,
            sh,
            *width,
            *height,
            *fit,
            *without_enlargement,
            *without_reduction,
        )
    });
    let plan = super::resize_plan(spec.fed, (rw, rh), scales);
    let g = Geometry::new(spec.fed, (rw, rh), plan.as_ref(), spec.has_alpha);
    let options = super::resize_options(*kernel, spec.has_alpha);
    let kind = if spec.wide {
        g.streamable::<u16>()
            .then(|| Kind::Wide(Engine::new(g, options, narrow)))?
    } else {
        g.streamable::<u8>()
            .then(|| Kind::Narrow(Engine::new(g, options, false)))?
    };
    Some(Box::new(ResizeSink {
        kind,
        out: (rw, rh),
        has_alpha: spec.has_alpha,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastpkg_core::options::Fit;

    fn noise<T: Sample + From<u8>>(n: usize, seed: u64) -> Vec<T> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                T::try_from((state >> 33) % (u8::MAX as u64 + 1)).unwrap_or_default()
            })
            .collect()
    }

    /// Plain integer division, as the first version of the box shrink did it.
    fn naive_box<T: Sample>(
        data: &[T],
        (width, height): (usize, usize),
        (kx, ky): (usize, usize),
        weighted: bool,
    ) -> Vec<T> {
        let (out_w, out_h) = (width.div_ceil(kx), height.div_ceil(ky));
        let area = (kx * ky) as u64;
        let mut out = Vec::new();
        for oy in 0..out_h {
            for ox in 0..out_w {
                let mut acc = [0u64; 4];
                let mut weight = 0u64;
                for dy in 0..ky {
                    for dx in 0..kx {
                        let y = (oy * ky + dy).min(height - 1);
                        let x = (ox * kx + dx).min(width - 1);
                        let px = &data[(y * width + x) * 4..][..4];
                        let a: u64 = px[3].into();
                        weight += a;
                        for c in 0..3 {
                            let v: u64 = px[c].into();
                            acc[c] += if weighted { v * a } else { v };
                        }
                        acc[3] += a;
                    }
                }
                for c in 0..3 {
                    let v = if weighted {
                        (acc[c] + weight / 2).checked_div(weight).unwrap_or(0)
                    } else {
                        (acc[c] + area / 2) / area
                    };
                    out.push(T::try_from(v).unwrap_or_default());
                }
                out.push(T::try_from((acc[3] + area / 2) / area).unwrap_or_default());
            }
        }
        out
    }

    #[test]
    fn box_shrink_is_the_plain_block_mean() {
        for (w, h, kx, ky) in [
            (37, 29, 3, 2),
            (64, 8, 2, 2),
            (5, 5, 1, 4),
            (9, 3, 7, 1),
            (1, 1, 2, 2),
            (40, 41, 16, 16),
        ] {
            for weighted in [false, true] {
                let a: Vec<u8> = noise(w * h * 4, 3);
                assert_eq!(
                    box_shrink(&a, (w, h), (kx, ky), weighted).0,
                    naive_box(&a, (w, h), (kx, ky), weighted),
                    "u8 {w}x{h} {kx}x{ky} weighted {weighted}"
                );
                // 16-bit values spread over the whole range, big blocks included.
                let b: Vec<u16> = noise::<u16>(w * h * 4, 9)
                    .iter()
                    .map(|v| v.wrapping_mul(257).wrapping_add(*v * 3))
                    .collect();
                assert_eq!(
                    box_shrink(&b, (w, h), (kx, ky), weighted).0,
                    naive_box(&b, (w, h), (kx, ky), weighted),
                    "u16 {w}x{h} {kx}x{ky} weighted {weighted}"
                );
            }
        }
    }

    /// The resize the whole frame goes through (`resize_raw_opts` for small frames).
    fn one_shot<T: Sample>(
        data: &[T],
        fed: (u32, u32),
        out: (u32, u32),
        plan: Option<&crate::ResizePlan>,
        kernel: Kernel,
        alpha: bool,
    ) -> Vec<T> {
        let mut options = crate::resize_options(kernel, alpha);
        let mut crop = [0.0, 0.0, fed.0 as f64, fed.1 as f64];
        let (data, size) = match plan {
            Some(plan) => {
                crop = plan.crop;
                let [l, t, w, h] = plan.crop;
                options = options.crop(l, t, w, h);
                crate::prepare_source(data.to_vec(), fed, plan, alpha)
            }
            None => (data.to_vec(), fed),
        };
        let src = ImageRef::new(size.0, size.1, bytemuck::cast_slice(&data), T::PIXEL).unwrap();
        let mut dst = vec![T::default(); out.0 as usize * out.1 as usize * 4];
        let mut view =
            FirImage::from_slice_u8(out.0, out.1, bytemuck::cast_slice_mut(&mut dst), T::PIXEL)
                .unwrap();
        crate::resize_banded(&src, &mut view, &options, crop, true, usize::MAX).unwrap();
        dst
    }

    /// Streaming gives the one-shot result (to float rounding) for plain shrinks of both depths,
    /// with and without alpha, with and without a box factor and padding, in several bands.
    #[test]
    fn stream_matches_one_shot() {
        for (sw, sh, width, height, fit) in [
            (300u32, 500u32, Some(90u32), None, Fit::Cover),
            (640, 480, Some(100), Some(61), Fit::Cover),
            (257, 331, Some(257), Some(100), Fit::Fill),
            (800, 120, Some(97), None, Fit::Cover),
            (333, 777, None, Some(41), Fit::Cover),
            (96, 96, Some(31), Some(31), Fit::Inside),
        ] {
            for kernel in [Kernel::Lanczos3, Kernel::Cubic] {
                for alpha in [false, true] {
                    let (rw, rh) = fastpkg_core::pipeline::compute_target(
                        sw, sh, width, height, fit, false, false,
                    );
                    let scales = fastpkg_core::pipeline::resolve_shrink(
                        sw, sh, width, height, fit, false, false,
                    );
                    let plan = crate::resize_plan((sw, sh), (rw, rh), Some(scales));
                    let g = Geometry::new((sw, sh), (rw, rh), plan.as_ref(), alpha);
                    if !g.streamable::<u8>() {
                        continue;
                    }
                    let options = crate::resize_options(kernel, alpha);
                    // 8-bit
                    let data: Vec<u8> = noise(sw as usize * sh as usize * 4, 5);
                    let want = one_shot(&data, (sw, sh), (rw, rh), plan.as_ref(), kernel, alpha);
                    for window in [1usize << 12, 1 << 16] {
                        let mut engine =
                            Engine::<u8>::with_window(g.clone(), options, false, window);
                        for row in data.chunks_exact(sw as usize * 4) {
                            engine.push_row(row).unwrap();
                        }
                        let Output::Same(got) = engine.finish().unwrap() else {
                            panic!()
                        };
                        let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max();
                        assert!(worst <= Some(1), "u8 {sw}x{sh}->{rw}x{rh} {kernel:?} alpha {alpha} window {window}: {worst:?}");
                    }
                    // 16-bit, narrowed and not
                    let wide: Vec<u16> = noise::<u16>(sw as usize * sh as usize * 4, 6)
                        .iter()
                        .map(|v| v.wrapping_mul(257))
                        .collect();
                    let want = one_shot(&wide, (sw, sh), (rw, rh), plan.as_ref(), kernel, alpha);
                    for narrow in [false, true] {
                        let mut engine =
                            Engine::<u16>::with_window(g.clone(), options, narrow, 1 << 13);
                        for row in wide.chunks_exact(sw as usize * 4) {
                            engine.push_row(row).unwrap();
                        }
                        match engine.finish().unwrap() {
                            Output::Same(got) => {
                                assert!(!narrow);
                                let worst =
                                    got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max();
                                assert!(
                                    worst <= Some(2),
                                    "u16 {sw}x{sh}->{rw}x{rh} {kernel:?} alpha {alpha}: {worst:?}"
                                );
                            }
                            Output::Narrowed(got) => {
                                assert!(narrow);
                                let worst = got
                                    .iter()
                                    .zip(&want)
                                    .map(|(a, b)| a.abs_diff(narrow_sample(*b)))
                                    .max();
                                assert!(worst <= Some(1), "narrowed {sw}x{sh}->{rw}x{rh} {kernel:?} alpha {alpha}: {worst:?}");
                            }
                        }
                    }
                    // A frame in memory goes through the parallel box shrink.
                    let got = resize_frame(&data, g.clone(), options).unwrap();
                    let want = one_shot(&data, (sw, sh), (rw, rh), plan.as_ref(), kernel, alpha);
                    let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max();
                    assert!(
                        worst <= Some(1),
                        "frame {sw}x{sh}->{rw}x{rh} {kernel:?} alpha {alpha}: {worst:?}"
                    );
                }
            }
        }
    }
}
