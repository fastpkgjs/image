use std::time::Instant;

use fastpkg_codecs::{decode_with_hint, encode, probe};
use fastpkg_core::options::{Operation, OutputOptions, PipelineSpec};
use fastpkg_core::Format;
use fastpkg_ops::apply;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/large.jpg".into());
    let bytes = std::fs::read(&path).unwrap();
    let spec = PipelineSpec::default();
    let start = Instant::now();
    let header = probe(&bytes, &spec.input).unwrap();
    println!(
        "probe: {:?} {}x{}",
        start.elapsed(),
        header.width,
        header.height
    );

    let start = Instant::now();
    let image = decode_with_hint(
        &bytes,
        header.format,
        &spec.input,
        Some(fastpkg_codecs::ShrinkHint {
            target: (1200, 900),
            shrink: 4.0 / 3.0,
        }),
        true,
    )
    .unwrap();
    println!(
        "decode: {:?} {}x{}",
        start.elapsed(),
        image.width,
        image.height
    );

    let start = Instant::now();
    let resized = apply(
        image.clone(),
        &[Operation::Resize {
            width: Some(1200),
            height: None,
            fit: fastpkg_core::options::Fit::Cover,
            position: "centre".into(),
            background: "#000000".into(),
            kernel: fastpkg_core::options::Kernel::Lanczos3,
            without_enlargement: false,
            without_reduction: false,
            fast_shrink_on_load: true,
            defer_canvas: false,
        }],
    )
    .unwrap();
    println!(
        "resize: {:?} {}x{}",
        start.elapsed(),
        resized.width,
        resized.height
    );

    for format in [Format::Jpeg, Format::Png, Format::Webp] {
        let options = OutputOptions {
            format: Some(format),
            quality: 85,
            ..Default::default()
        };
        let start = Instant::now();
        let (data, _) = encode(&resized, &options, header.format).unwrap();
        println!(
            "encode {format:?}: {:?} {} bytes",
            start.elapsed(),
            data.len()
        );
    }
}
