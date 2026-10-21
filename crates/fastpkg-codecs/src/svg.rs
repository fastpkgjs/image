use fastpkg_core::options::TextInput;
use fastpkg_core::{FastpkgError, HeaderInfo, Image, Result};

pub fn probe(bytes: &[u8]) -> Result<HeaderInfo> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|e| FastpkgError::Decode(format!("svg: {e}")))?;
    let size = tree.size();
    Ok(HeaderInfo {
        format: fastpkg_core::Format::Svg,
        width: size.width().round().max(1.0) as u32,
        height: size.height().round().max(1.0) as u32,
        channels: 4,
        has_alpha: true,
        depth: "8-bit".into(),
        space: "srgb".into(),
        pages: 1,
        page_height: size.height().round().max(1.0) as u32,
        size: bytes.len(),
        ..Default::default()
    })
}

pub fn decode(bytes: &[u8]) -> Result<Image> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|e| FastpkgError::Decode(format!("svg: {e}")))?;
    let size = tree.size();
    let width = size.width().round().max(1.0) as u32;
    let height = size.height().round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| FastpkgError::Decode("svg: canvas too large".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let data = pixmap.take();
    let mut image = Image::from_rgba(width, height, data)?;
    image.unpremultiply();
    image.format = fastpkg_core::Format::Svg;
    Ok(image)
}

pub fn text_image(options: &TextInput) -> Result<Image> {
    let text = escape_xml(&options.text);
    let width = options.width.unwrap_or(600);
    let height = options.height.unwrap_or(200);
    let font_size = parse_font_size(options.font.as_deref()).unwrap_or(32.0);
    let family = parse_font_family(options.font.as_deref()).unwrap_or_else(|| "Arial".into());
    let anchor = match options.align.as_deref().unwrap_or("left") {
        "center" | "centre" => "middle",
        "right" => "end",
        _ => "start",
    };
    let x = match anchor {
        "middle" => width as f64 / 2.0,
        "end" => width as f64 - 8.0,
        _ => 8.0,
    };
    let y = ((height as f64 / 2.0) + font_size * 0.35).round();
    let fill = if options.rgba {
        "rgba(255,255,255,1)"
    } else {
        "#ffffff"
    };
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><rect width="100%" height="100%" fill="rgba(0,0,0,0)"/><text x="{x}" y="{y}" font-family="{family}" font-size="{font_size}" text-anchor="{anchor}" fill="{fill}">{text}</text></svg>"#
    );
    decode(svg.as_bytes())
}

fn parse_font_size(font: Option<&str>) -> Option<f64> {
    font?
        .split_whitespace()
        .find_map(|part| part.parse::<f64>().ok())
}

fn parse_font_family(font: Option<&str>) -> Option<String> {
    let font = font?;
    let family: String = font
        .split_whitespace()
        .filter(|part| part.parse::<f64>().is_err())
        .collect::<Vec<_>>()
        .join(" ");
    if family.is_empty() {
        None
    } else {
        Some(family)
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
