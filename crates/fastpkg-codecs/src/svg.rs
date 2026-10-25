use fastpkg_core::options::TextInput;
use fastpkg_core::{FastpkgError, HeaderInfo, Image, Result};

/// Maximum element nesting accepted (libxml2, and therefore librsvg/sharp, rejects deeper
/// documents; resvg recurses per level and overflows the stack on pathological nesting).
const MAX_NESTING: usize = 257;
/// Stack for SVG parse/render work; usvg/resvg recurse over the tree.
const SVG_STACK: usize = 64 * 1024 * 1024;

/// Cheap tag-depth scan: returns the maximum element nesting depth of `bytes`.
pub(crate) fn max_nesting(bytes: &[u8]) -> usize {
    let (mut depth, mut max, mut i) = (0usize, 0usize, 0usize);
    let n = bytes.len();
    while i < n {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i += 4 + find(&rest[4..], b"-->").map_or(n, |p| p + 3);
        } else if rest.starts_with(b"<![CDATA[") {
            i += 9 + find(&rest[9..], b"]]>").map_or(n, |p| p + 3);
        } else if rest.starts_with(b"<?") {
            i += 2 + find(&rest[2..], b"?>").map_or(n, |p| p + 2);
        } else if rest.starts_with(b"<!") {
            // DOCTYPE may contain a bracketed internal subset with nested `>`.
            let (mut j, mut bracket) = (2, 0usize);
            while j < rest.len() {
                match rest[j] {
                    b'[' => bracket += 1,
                    b']' => bracket = bracket.saturating_sub(1),
                    b'>' if bracket == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            i += j + 1;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i += 2 + rest[2..]
                .iter()
                .position(|&c| c == b'>')
                .map_or(n, |p| p + 1);
        } else {
            let (mut j, mut quote, mut self_close) = (1, 0u8, false);
            while j < rest.len() {
                let c = rest[j];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    self_close = rest[j - 1] == b'/';
                    break;
                }
                j += 1;
            }
            depth += 1;
            max = max.max(depth);
            if self_close {
                depth -= 1;
            }
            i += j + 1;
        }
    }
    max
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn check_nesting(bytes: &[u8]) -> Result<()> {
    if max_nesting(bytes) > MAX_NESTING {
        return Err(FastpkgError::Decode("svg: element nesting too deep".into()));
    }
    check_filter_cost(bytes)
}

/// Largest value of `attr` on any element whose tag name contains `tag`, or `None`.
fn max_tag_attr(bytes: &[u8], tag: &[u8], attr: &[u8]) -> Option<f64> {
    let mut best: Option<f64> = None;
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], tag) {
        let start = from + at + tag.len();
        from = start;
        let end = bytes[start..]
            .iter()
            .position(|&c| c == b'>')
            .map_or(bytes.len(), |p| start + p);
        let element = &bytes[start..end];
        let Some(i) = find(element, attr) else {
            continue;
        };
        let rest = element[i + attr.len()..].trim_ascii_start();
        let Some(rest) = rest.strip_prefix(b"=") else {
            continue;
        };
        let rest = rest.trim_ascii_start();
        let Some((&quote, rest)) = rest.split_first() else {
            continue;
        };
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        let value: String = rest
            .iter()
            .take_while(|&&c| c != quote)
            .map(|&c| c as char)
            .collect();
        for number in value.split(|c: char| c.is_whitespace() || c == ',') {
            if let Ok(n) = number.parse::<f64>() {
                best = Some(best.map_or(n, |b| b.max(n)));
            }
        }
    }
    best
}

/// resvg's filter primitives cost O(radius) or O(order^2) per pixel (feMorphology,
/// feConvolveMatrix, feTurbulence octaves); a few hundred bytes of SVG can pin a core for
/// minutes. Real documents stay far below these limits.
fn check_filter_cost(bytes: &[u8]) -> Result<()> {
    const LIMITS: [(&[u8], &[u8], f64, &str); 3] = [
        (b"feMorphology", b"radius", 256.0, "feMorphology radius"),
        (
            b"feConvolveMatrix",
            b"order",
            32.0,
            "feConvolveMatrix order",
        ),
        (
            b"feTurbulence",
            b"numOctaves",
            32.0,
            "feTurbulence numOctaves",
        ),
    ];
    for (tag, attr, limit, what) in LIMITS {
        if max_tag_attr(bytes, tag, attr).is_some_and(|value| value > limit) {
            return Err(FastpkgError::Decode(format!(
                "svg: {what} exceeds the supported limit of {limit}"
            )));
        }
    }
    Ok(())
}

/// Run `f` on a thread with a large stack so deep-but-legal documents cannot overflow it.
fn with_big_stack<T: Send>(f: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(SVG_STACK)
            .spawn_scoped(scope, f)
            .map_err(|e| FastpkgError::Decode(format!("svg: cannot spawn worker: {e}")))?
            .join()
            .unwrap_or_else(|_| Err(FastpkgError::Decode("svg: renderer panicked".into())))
    })
}

/// SVG sizes are in CSS pixels (72 per inch); `density` rasterises at that many dots per inch.
fn scaled(size: f32, density: u32) -> u32 {
    (size as f64 * density as f64 / 72.0).round().max(1.0) as u32
}

pub fn probe(bytes: &[u8], density: u32) -> Result<HeaderInfo> {
    check_nesting(bytes)?;
    with_big_stack(|| probe_inner(bytes, density))
}

fn probe_inner(bytes: &[u8], density: u32) -> Result<HeaderInfo> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|e| FastpkgError::Decode(format!("svg: {e}")))?;
    let size = tree.size();
    Ok(HeaderInfo {
        format: fastpkg_core::Format::Svg,
        width: scaled(size.width(), density),
        height: scaled(size.height(), density),
        channels: 4,
        has_alpha: true,
        depth: "uchar".into(),
        space: "srgb".into(),
        pages: 1,
        page_height: scaled(size.height(), density),
        size: bytes.len(),
        ..Default::default()
    })
}

pub fn decode(bytes: &[u8], density: u32) -> Result<Image> {
    check_nesting(bytes)?;
    with_big_stack(|| decode_inner(bytes, density))
}

fn decode_inner(bytes: &[u8], density: u32) -> Result<Image> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|e| FastpkgError::Decode(format!("svg: {e}")))?;
    let size = tree.size();
    let width = scaled(size.width(), density);
    let height = scaled(size.height(), density);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| FastpkgError::Decode("svg: canvas too large".into()))?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
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
    decode(svg.as_bytes(), 72)
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
