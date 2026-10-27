//! Row-by-row hand-off from a decoder to a consumer that resamples while the image is still being
//! decoded, so the full-size frame never exists in memory.

use crate::{Image, Result};

/// What a decoder tells the sink factory about the rows it is going to push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSinkSpec {
    /// Size of the image in the file.
    pub source: (u32, u32),
    /// Size of the rows that are pushed: `source`, or the decoder's own area-average shrink of it.
    pub fed: (u32, u32),
    /// The file has an alpha channel (colour is premultiplied while resampling).
    pub has_alpha: bool,
    /// Samples are 16-bit (`push_u16`) rather than 8-bit (`push_u8`).
    pub wide: bool,
}

/// Consumer of RGBA rows in top-to-bottom order: exactly `fed.1` rows of `fed.0` pixels, then
/// `finish`. The result is the finished (resampled) image.
pub trait RowSink {
    fn push_u8(&mut self, row: &[u8]) -> Result<()>;
    fn push_u16(&mut self, row: &[u16]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<Image>;
}

/// Builds the sink for one decode attempt, or declines (`None`: decode as usual). Called once per
/// attempt, because a failed attempt may be retried on repaired data.
pub type SinkFactory<'a> = &'a dyn Fn(&RowSinkSpec) -> Option<Box<dyn RowSink>>;
