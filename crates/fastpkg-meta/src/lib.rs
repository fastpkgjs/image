use std::io::Cursor;

use fastpkg_core::{Format, HeaderInfo};
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataExtras {
    #[serde(skip)]
    pub exif_raw: Option<Vec<u8>>,
    #[serde(skip)]
    pub icc_raw: Option<Vec<u8>>,
    #[serde(skip)]
    pub xmp_raw: Option<Vec<u8>>,
    pub orientation: Option<u32>,
    pub density: Option<u32>,
    pub exif: Option<Value>,
    pub icc: Option<String>,
    pub xmp: Option<String>,
    pub comments: Option<String>,
}

pub fn probe(bytes: &[u8]) -> MetadataExtras {
    MetadataExtras {
        orientation: read_orientation(bytes),
        ..Default::default()
    }
}

pub fn extract(bytes: &[u8], _format: Format) -> MetadataExtras {
    MetadataExtras {
        orientation: read_orientation(bytes),
        exif: read_exif(bytes),
        exif_raw: read_exif_raw(bytes),
        icc_raw: read_icc_raw(bytes),
        xmp_raw: read_xmp_raw(bytes),
        xmp: read_xmp(bytes),
        icc: read_icc(bytes),
        comments: read_comment(bytes),
        ..Default::default()
    }
}

pub fn enrich(header: &mut HeaderInfo, extras: &MetadataExtras) {
    header.orientation = extras.orientation;
    header.density = extras.density;
    header.has_exif = header.has_exif || extras.exif.is_some() || extras.exif_raw.is_some();
    header.has_profile = header.has_profile || extras.icc.is_some() || extras.icc_raw.is_some();
    header.has_xmp = header.has_xmp || extras.xmp.is_some() || extras.xmp_raw.is_some();
}

fn read_orientation(bytes: &[u8]) -> Option<u32> {
    let exif = read_exif_reader(bytes)?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
}

fn read_exif(bytes: &[u8]) -> Option<Value> {
    let exif = read_exif_reader(bytes)?;
    let mut map = Map::new();
    for field in exif.fields() {
        let key = format!("{:?}", field.tag);
        let value = field.display_value().with_unit(&exif).to_string();
        map.insert(key, Value::String(value));
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

fn read_exif_reader(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = Cursor::new(bytes);
    exif::Reader::new().read_from_container(&mut cursor).ok()
}

fn read_xmp_raw(bytes: &[u8]) -> Option<Vec<u8>> {
    let start = find(bytes, b"<x:xmpmeta")?;
    let end = find(&bytes[start..], b"</x:xmpmeta>")? + start + b"</x:xmpmeta>".len();
    Some(bytes[start..end].to_vec())
}

fn read_xmp(bytes: &[u8]) -> Option<String> {
    read_xmp_raw(bytes).and_then(|raw| String::from_utf8(raw).ok())
}

fn read_exif_raw(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() >= 4 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        let mut i = 2;
        while i + 4 <= bytes.len() {
            if bytes[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xd9 || marker == 0xda {
                break;
            }
            if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if len < 2 || i + 2 + len > bytes.len() {
                break;
            }
            let data = &bytes[i + 4..i + 2 + len];
            if marker == 0xe1 && data.starts_with(b"Exif\x00\x00") {
                return Some(data[6..].to_vec());
            }
            i += 2 + len;
        }
    }
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        let mut offset = 8;
        while offset + 12 <= bytes.len() {
            let len = u32::from_be_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]) as usize;
            let kind = &bytes[offset + 4..offset + 8];
            let start = offset + 8;
            let end = start.saturating_add(len).min(bytes.len());
            if kind == b"eXIf" {
                return Some(bytes[start..end].to_vec());
            }
            if kind == b"IEND" {
                break;
            }
            offset = end.saturating_add(4);
        }
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let mut offset = 12;
        while offset + 8 <= bytes.len() {
            let kind = &bytes[offset..offset + 4];
            let len = u32::from_le_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]) as usize;
            let start = offset + 8;
            let end = start.saturating_add(len).min(bytes.len());
            if kind == b"EXIF" {
                return Some(bytes[start..end].to_vec());
            }
            offset = end.saturating_add(len & 1);
        }
    }
    None
}

fn read_icc_raw(bytes: &[u8]) -> Option<Vec<u8>> {
    if let Some(raw) = read_jpeg_icc_raw(bytes) {
        return Some(raw);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let mut offset = 12;
        while offset + 8 <= bytes.len() {
            let kind = &bytes[offset..offset + 4];
            let len = u32::from_le_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]) as usize;
            let start = offset + 8;
            let end = start.saturating_add(len).min(bytes.len());
            if kind == b"ICCP" {
                return Some(bytes[start..end].to_vec());
            }
            offset = end.saturating_add(len & 1);
        }
    }
    None
}

fn read_icc(bytes: &[u8]) -> Option<String> {
    read_icc_raw(bytes).map(|raw| base64_encode(&raw))
}

fn read_jpeg_icc_raw(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut i = 2;
    let marker = b"ICC_PROFILE\x00";
    let mut chunks: Vec<(u8, Vec<u8>)> = Vec::new();
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let m = bytes[i + 1];
        if m == 0xd9 || m == 0xda {
            break;
        }
        if (0xd0..=0xd7).contains(&m) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if len < 2 || i + 2 + len > bytes.len() {
            break;
        }
        let data = &bytes[i + 4..i + 2 + len];
        if m == 0xe2 && data.starts_with(marker) && data.len() > marker.len() + 2 {
            let sequence = data[marker.len()];
            chunks.push((sequence, data[marker.len() + 2..].to_vec()));
        }
        i += 2 + len;
    }
    if chunks.is_empty() {
        return None;
    }
    chunks.sort_by_key(|(sequence, _)| *sequence);
    Some(chunks.into_iter().flat_map(|(_, chunk)| chunk).collect())
}

fn read_comment(bytes: &[u8]) -> Option<String> {
    let start = find(bytes, b"Comment\x00")? + 8;
    let end = bytes[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| start + p)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(str::to_owned)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
