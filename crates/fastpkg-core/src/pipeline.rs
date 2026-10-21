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

pub fn compute_target(
    width: u32,
    height: u32,
    target_width: Option<u32>,
    target_height: Option<u32>,
    fit: Fit,
    without_enlargement: bool,
    without_reduction: bool,
) -> (u32, u32) {
    let (w, h) = match (target_width, target_height) {
        (Some(w), Some(h)) => match fit {
            Fit::Fill => (w, h),
            Fit::Contain | Fit::Inside => {
                let scale = (w as f64 / width as f64).min(h as f64 / height as f64);
                (
                    (width as f64 * scale).round() as u32,
                    (height as f64 * scale).round() as u32,
                )
            }
            Fit::Outside => {
                let scale = (w as f64 / width as f64).max(h as f64 / height as f64);
                (
                    (width as f64 * scale).round() as u32,
                    (height as f64 * scale).round() as u32,
                )
            }
            Fit::Cover => (w, h),
        },
        (Some(w), None) => {
            let scale = w as f64 / width as f64;
            (w, (height as f64 * scale).round() as u32)
        }
        (None, Some(h)) => {
            let scale = h as f64 / height as f64;
            ((width as f64 * scale).round() as u32, h)
        }
        (None, None) => (width, height),
    };
    let mut out = (w.max(1), h.max(1));
    if without_enlargement && (out.0 > width || out.1 > height) {
        let scale = (width as f64 / out.0 as f64).min(height as f64 / out.1 as f64);
        out = (
            (out.0 as f64 * scale).round().max(1.0) as u32,
            (out.1 as f64 * scale).round().max(1.0) as u32,
        );
    }
    if without_reduction && (out.0 < width || out.1 < height) {
        let scale = (width as f64 / out.0 as f64).max(height as f64 / out.1 as f64);
        out = (
            (out.0 as f64 * scale).round().max(1.0) as u32,
            (out.1 as f64 * scale).round().max(1.0) as u32,
        );
    }
    out
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
