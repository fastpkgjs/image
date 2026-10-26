//! Header-only readers used by `probe`/`metadata()`: none of them decode pixel data.

/// Pixels per inch from a JFIF APP0 segment (units 1 = dpi, 2 = dots per cm, 0 = aspect only).
fn jfif_density(data: &[u8]) -> Option<u32> {
    if data.len() < 12 || !data.starts_with(b"JFIF\0") {
        return None;
    }
    let x = u16::from_be_bytes([data[8], data[9]]) as f64;
    match data[7] {
        1 => Some(x as u32),
        2 => Some((x * 2.54).round() as u32),
        _ => None,
    }
    .filter(|dpi| *dpi > 25)
}

#[derive(Debug, Default, Clone)]
pub struct JpegDetails {
    /// `4:2:0`, `4:4:4`, ... derived from the luma sampling factors of the frame header.
    pub chroma_subsampling: Option<String>,
    pub density: Option<u32>,
    pub has_jfif: bool,
}

pub fn jpeg_details(bytes: &[u8]) -> JpegDetails {
    let mut details = JpegDetails::default();
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return details;
    }
    let mut offset = 2;
    while offset + 4 <= bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = bytes[offset + 1];
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) || marker == 0xff {
            offset += if marker == 0xff { 1 } else { 2 };
            continue;
        }
        let length = u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]]) as usize;
        if length < 2 || offset + 2 + length > bytes.len() {
            break;
        }
        let data = &bytes[offset + 4..offset + 2 + length];
        if marker == 0xe0 && data.starts_with(b"JFIF\0") {
            details.has_jfif = true;
            details.density = jfif_density(data);
        } else if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            // precision(1) height(2) width(2) components(1) then id, sampling, table per component
            if data.len() >= 9 {
                let sampling = data[7];
                let (h, v) = (sampling >> 4, sampling & 15);
                details.chroma_subsampling = match (h, v) {
                    (1, 1) => Some("4:4:4"),
                    (2, 1) => Some("4:2:2"),
                    (1, 2) => Some("4:4:0"),
                    (2, 2) => Some("4:2:0"),
                    (4, 1) => Some("4:1:1"),
                    (4, 2) => Some("4:1:0"),
                    _ => None,
                }
                .map(str::to_string);
            }
        }
        offset += 2 + length;
    }
    details
}

#[derive(Debug, Default, Clone)]
pub struct PngDetails {
    pub interlaced: bool,
    pub bit_depth: u8,
    pub color_type: u8,
    pub density: Option<u32>,
    pub palette_bit_depth: Option<u8>,
    pub transparency: bool,
}

pub fn png_details(bytes: &[u8]) -> PngDetails {
    let mut details = PngDetails::default();
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return details;
    }
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
        let data = &bytes[start..end];
        match kind {
            b"IHDR" if data.len() >= 13 => {
                details.bit_depth = data[8];
                details.color_type = data[9];
                details.interlaced = data[12] == 1;
                if data[9] == 3 {
                    details.palette_bit_depth = Some(data[8]);
                }
            }
            b"pHYs" if data.len() >= 9 && data[8] == 1 => {
                // sharp only reports a density above libvips' default of 1 pixel/mm.
                let ppm = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                if ppm > 1000 {
                    details.density = Some((ppm as f64 * 0.0254).round() as u32);
                }
            }
            b"tRNS" => details.transparency = true,
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        offset = end.saturating_add(4);
    }
    details
}

#[derive(Debug, Default, Clone)]
pub struct WebpDetails {
    pub alpha: bool,
    pub animated: bool,
    pub frames: u32,
    /// Frame durations in milliseconds.
    pub delays: Vec<u32>,
    pub loop_count: Option<u32>,
    pub has_exif: bool,
}

pub fn webp_details(bytes: &[u8]) -> WebpDetails {
    let mut details = WebpDetails::default();
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return details;
    }
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
        let data = &bytes[start..end];
        match kind {
            b"VP8X" if !data.is_empty() => {
                details.alpha |= data[0] & 0x10 != 0;
                details.animated |= data[0] & 0x02 != 0;
            }
            b"VP8L" if data.len() >= 5 && data[0] == 0x2f => {
                // signature, 14 bits width-1, 14 bits height-1, 1 bit alpha_is_used
                details.alpha |= data[4] & 0x10 != 0;
            }
            b"ALPH" => details.alpha = true,
            b"ANIM" if data.len() >= 6 => {
                details.loop_count = Some(u16::from_le_bytes([data[4], data[5]]) as u32);
            }
            b"ANMF" if data.len() >= 16 => {
                details.frames += 1;
                let duration = u32::from_le_bytes([data[12], data[13], data[14], 0]);
                details.delays.push(duration);
                // Frame payload: fourcc(4) size(4) then the bitstream; VP8L carries an alpha bit.
                if data.len() > 28 && &data[16..20] == b"VP8L" && data[24] == 0x2f {
                    details.alpha |= data[28] & 0x10 != 0;
                }
            }
            b"EXIF" => details.has_exif = true,
            _ => {}
        }
        offset = offset.saturating_add(8 + len + (len & 1));
    }
    details
}

#[derive(Debug, Default, Clone)]
pub struct GifDetails {
    pub transparent: bool,
    pub frames: u32,
    /// Frame delays in milliseconds (GIF stores centiseconds).
    pub delays: Vec<u32>,
    pub loop_count: Option<u32>,
}

pub fn gif_details(bytes: &[u8]) -> GifDetails {
    let mut details = GifDetails::default();
    if bytes.len() < 13 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return details;
    }
    let skip_sub_blocks = |mut at: usize| -> usize {
        while at < bytes.len() {
            let size = bytes[at] as usize;
            at += 1 + size;
            if size == 0 {
                break;
            }
        }
        at
    };
    let flags = bytes[10];
    let mut at = 13;
    if flags & 0x80 != 0 {
        at += 3 * (1usize << ((flags & 7) + 1));
    }
    let mut pending_delay = 0u32;
    while at < bytes.len() {
        match bytes[at] {
            0x21 if at + 2 <= bytes.len() => {
                let label = bytes[at + 1];
                if label == 0xf9 && at + 8 <= bytes.len() {
                    // graphic control extension: size(4) flags delay(2) transparent-index terminator
                    let gce_flags = bytes[at + 3];
                    pending_delay = u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]) as u32 * 10;
                    details.transparent |= gce_flags & 1 != 0;
                } else if label == 0xff
                    && at + 16 <= bytes.len()
                    && &bytes[at + 3..at + 14] == b"NETSCAPE2.0"
                    && bytes[at + 14] == 3
                {
                    details.loop_count =
                        Some(u16::from_le_bytes([bytes[at + 16], bytes[at + 17]]) as u32);
                }
                at = skip_sub_blocks(at + 2);
            }
            0x2c => {
                details.frames += 1;
                details.delays.push(pending_delay);
                pending_delay = 0;
                if at + 10 > bytes.len() {
                    break;
                }
                let image_flags = bytes[at + 9];
                at += 10;
                if image_flags & 0x80 != 0 {
                    at += 3 * (1usize << ((image_flags & 7) + 1));
                }
                at += 1; // LZW minimum code size
                at = skip_sub_blocks(at);
            }
            0x3b => break,
            _ => break,
        }
    }
    details
}

/// AVIF stores alpha as an auxiliary image identified by one of these URNs in the `meta` box.
pub fn avif_has_alpha(bytes: &[u8]) -> bool {
    const URNS: [&[u8]; 2] = [
        b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
        b"urn:mpeg:hevc:2015:auxid:1",
    ];
    let head = &bytes[..bytes.len().min(16 * 1024)];
    URNS.iter()
        .any(|urn| head.windows(urn.len()).any(|window| window == *urn))
}

/// Whether the file ends inside a block (a cut header, colour table, descriptor or an
/// unterminated sub-block chain). A file that merely lacks its trailer is not truncated.
pub fn gif_truncated(bytes: &[u8]) -> bool {
    if bytes.len() < 13 {
        return true;
    }
    let blocks_complete = |mut p: usize| -> Option<usize> {
        while p < bytes.len() {
            let size = bytes[p] as usize;
            p += 1 + size;
            if size == 0 {
                return Some(p);
            }
        }
        None
    };
    let mut at = 13;
    if bytes[10] & 0x80 != 0 {
        at += 3 * (1usize << ((bytes[10] & 7) + 1));
    }
    if at > bytes.len() {
        return true;
    }
    while at < bytes.len() {
        match bytes[at] {
            0x21 => match at.checked_add(2).and_then(blocks_complete) {
                Some(end) => at = end,
                None => return true,
            },
            0x2c => {
                if at + 10 > bytes.len() {
                    return true;
                }
                let mut p = at + 10;
                if bytes[at + 9] & 0x80 != 0 {
                    p += 3 * (1usize << ((bytes[at + 9] & 7) + 1));
                }
                match blocks_complete(p + 1) {
                    Some(end) if p < bytes.len() => at = end,
                    _ => return true,
                }
            }
            _ => return false,
        }
    }
    false
}

/// Whether some frame has at least one complete, non-empty data sub-block: the minimum libvips
/// needs before it reports a (partly blank) frame instead of "no frames in GIF".
pub fn gif_has_frame_data(bytes: &[u8]) -> bool {
    let mut at = 13usize;
    if bytes.len() < 13 {
        return false;
    }
    if bytes[10] & 0x80 != 0 {
        at += 3 * (1usize << ((bytes[10] & 7) + 1));
    }
    while at + 2 <= bytes.len() {
        match bytes[at] {
            0x21 => {
                at += 2;
                loop {
                    let Some(&size) = bytes.get(at) else {
                        return false;
                    };
                    at += 1 + size as usize;
                    if size == 0 {
                        break;
                    }
                }
            }
            0x2c if at + 10 <= bytes.len() => {
                let mut p = at + 10;
                if bytes[at + 9] & 0x80 != 0 {
                    p += 3 * (1usize << ((bytes[at + 9] & 7) + 1));
                }
                return bytes
                    .get(p + 1)
                    .is_some_and(|&size| size > 0 && p + 2 + size as usize <= bytes.len());
            }
            _ => return false,
        }
    }
    false
}

/// Cuts a damaged GIF after its last complete sub-block, terminates the open block and appends
/// the trailer so that a lenient decoder can read the frames that survived.
pub fn gif_repair(bytes: &[u8]) -> Vec<u8> {
    let mut at = 13usize;
    let mut keep = bytes.len().min(13);
    if bytes.len() >= 13 && bytes[10] & 0x80 != 0 {
        at += 3 * (1usize << ((bytes[10] & 7) + 1));
    }
    keep = keep.max(at.min(bytes.len()));
    let blocks_end = |mut p: usize| -> (usize, bool) {
        // Returns the end of the sub-block chain starting at `p` and whether it is terminated.
        while p < bytes.len() {
            let size = bytes[p] as usize;
            if p + 1 + size > bytes.len() {
                return (p, false);
            }
            p += 1 + size;
            if size == 0 {
                return (p, true);
            }
        }
        (p, false)
    };
    while at < bytes.len() {
        match bytes[at] {
            0x21 if at + 2 <= bytes.len() => {
                let (end, done) = blocks_end(at + 2);
                if !done {
                    break;
                }
                at = end;
                keep = at;
            }
            0x2c if at + 10 <= bytes.len() => {
                let image_flags = bytes[at + 9];
                let mut p = at + 10;
                if image_flags & 0x80 != 0 {
                    p += 3 * (1usize << ((image_flags & 7) + 1));
                }
                p += 1;
                if p >= bytes.len() {
                    break;
                }
                let (end, done) = blocks_end(p);
                if done {
                    at = end;
                    keep = at;
                } else {
                    // Keep the complete sub-blocks of this frame and close the chain.
                    let mut out = bytes[..end].to_vec();
                    out.push(0);
                    out.push(0x3b);
                    return out;
                }
            }
            _ => break,
        }
    }
    let mut out = bytes[..keep.min(bytes.len())].to_vec();
    out.push(0x3b);
    out
}
