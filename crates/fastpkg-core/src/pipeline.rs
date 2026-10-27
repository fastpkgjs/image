use crate::options::{Fit, Kernel, Operation, OutputOptions, PipelineSpec};
use crate::{FastpkgError, Result};

pub fn validate_spec(spec: &PipelineSpec) -> Result<()> {
    if spec.input.raw.is_some() && spec.input.create.is_some() {
        return Err(FastpkgError::InvalidOptions(
            "raw and create inputs are mutually exclusive".into(),
        ));
    }
    for op in &spec.operations {
        validate_operation(op)?;
    }
    if let Some(timeout) = spec.output.timeout {
        if timeout <= 0.0 {
            return Err(FastpkgError::InvalidOptions(
                "timeout must be positive".into(),
            ));
        }
    }
    Ok(())
}

fn validate_operation(op: &Operation) -> Result<()> {
    match op {
        Operation::Extract { width, height, .. } => {
            if *width == 0 || *height == 0 {
                return Err(FastpkgError::InvalidOptions(
                    "extract width and height must be non-zero".into(),
                ));
            }
        }
        Operation::Convolve {
            width,
            height,
            kernel,
            ..
        } => {
            if *width == 0 || *height == 0 || kernel.len() != (*width as usize * *height as usize) {
                return Err(FastpkgError::InvalidOptions(
                    "convolve kernel dimensions do not match kernel length".into(),
                ));
            }
        }
        Operation::Recomb { matrix } => {
            if !(matrix.len() == 3 || matrix.len() == 4)
                || matrix.iter().any(|row| row.len() != matrix.len())
            {
                return Err(FastpkgError::InvalidOptions(
                    "recomb requires a square 3x3 or 4x4 matrix".into(),
                ));
            }
        }
        Operation::Affine { matrix, .. } => {
            if matrix.len() != 4 && matrix.len() != 6 {
                return Err(FastpkgError::InvalidOptions(
                    "affine requires a 2x2 or 2x3 matrix".into(),
                ));
            }
        }
        Operation::Clahe { width, height, .. } if *width == 0 || *height == 0 => {
            return Err(FastpkgError::InvalidOptions(
                "clahe window dimensions must be non-zero".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Per-axis shrink factors (source size over resampled size) of a resize, before any `cover`
/// crop or `contain` padding. Mirrors sharp's `ResolveShrink`: the factors are derived from the
/// fit, then `withoutReduction` clamps each to at most 1 and `withoutEnlargement` to at least 1
/// (when both flags are given only `withoutReduction` applies). `fill` with a single dimension
/// keeps the other axis.
pub fn resolve_shrink(
    width: u32,
    height: u32,
    target_width: Option<u32>,
    target_height: Option<u32>,
    fit: Fit,
    without_enlargement: bool,
    without_reduction: bool,
) -> (f64, f64) {
    let (iw, ih) = (width.max(1) as f64, height.max(1) as f64);
    let fill = fit == Fit::Fill;
    let (mut xf, mut yf) = (1.0f64, 1.0f64);
    match (target_width, target_height) {
        (Some(w), Some(h)) => {
            xf = iw / w.max(1) as f64;
            yf = ih / h.max(1) as f64;
            match fit {
                Fit::Cover | Fit::Outside => {
                    if xf < yf {
                        yf = xf;
                    } else {
                        xf = yf;
                    }
                }
                Fit::Contain | Fit::Inside => {
                    if xf > yf {
                        yf = xf;
                    } else {
                        xf = yf;
                    }
                }
                Fit::Fill => {}
            }
        }
        (Some(w), None) => {
            xf = iw / w.max(1) as f64;
            yf = if fill { 1.0 } else { xf };
        }
        (None, Some(h)) => {
            yf = ih / h.max(1) as f64;
            xf = if fill { 1.0 } else { yf };
        }
        (None, None) => {}
    }
    if without_reduction {
        xf = xf.min(1.0);
        yf = yf.min(1.0);
    } else if without_enlargement {
        xf = xf.max(1.0);
        yf = yf.max(1.0);
    }
    (xf, yf)
}

/// Size the source is resampled to before any `cover` crop or `contain` padding (see
/// [`resolve_shrink`]).
pub fn compute_target(
    width: u32,
    height: u32,
    target_width: Option<u32>,
    target_height: Option<u32>,
    fit: Fit,
    without_enlargement: bool,
    without_reduction: bool,
) -> (u32, u32) {
    let (xf, yf) = resolve_shrink(
        width,
        height,
        target_width,
        target_height,
        fit,
        without_enlargement,
        without_reduction,
    );
    (
        ((width.max(1) as f64 / xf).round() as u32).max(1),
        ((height.max(1) as f64 / yf).round() as u32).max(1),
    )
}

pub fn kernel_name(kernel: Kernel) -> &'static str {
    match kernel {
        Kernel::Nearest => "nearest",
        Kernel::Linear | Kernel::Bilinear => "bilinear",
        Kernel::Cubic => "bicubic",
        Kernel::Mitchell => "mitchell",
        Kernel::Lanczos2 => "lanczos2",
        Kernel::Lanczos3 => "lanczos3",
        Kernel::Mks2013 => "mks2013",
        Kernel::Mks2021 => "mks2021",
    }
}

pub fn default_output_for(input_format: crate::Format, options: &OutputOptions) -> crate::Format {
    options.resolved_format(input_format)
}
