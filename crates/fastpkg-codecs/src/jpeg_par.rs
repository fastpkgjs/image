//! Opt-in parallel baseline JPEG encoding (`jpeg({ parallel: true })`).
//!
//! The image is cut into horizontal bands that are compressed independently with the same
//! quantisation tables and the standard Huffman tables, then spliced into one stream whose
//! restart interval equals one band. A band's entropy-coded data starts with DC predictors at
//! zero and ends padded with 1-bits to a byte boundary, which is exactly the state a restart
//! marker resets to, so the result decodes to the same pixels as a single-pass encode with the
//! same settings.

use rayon::prelude::*;

/// Images below this many pixels use the normal single-pass encoder.
const MIN_PIXELS: usize = 1_000_000;
/// Smallest band; every band except the last is also a multiple of 16 rows, so no 8x8 block,
/// 4:2:0 chroma window or MCU straddles a boundary.
const MIN_BAND_ROWS: usize = 64;

/// Splits `height` rows into `(rows_per_band, band_count)`. Every band but the last has exactly
/// `rows_per_band` rows (a multiple of 16) and the last has between `MIN_BAND_ROWS` and
/// `rows_per_band` rows: the restart interval is fixed, so no band may be longer than the
/// first. `None` when the image is too small or only one thread is usable.
pub(crate) fn plan(width: usize, height: usize, threads: usize) -> Option<(usize, usize)> {
    if width.checked_mul(height)? < MIN_PIXELS {
        return None;
    }
    let (rows, count) = (2..=threads.min(height / MIN_BAND_ROWS))
        .rev()
        .find_map(|target| {
            let rows = height.div_ceil(target).next_multiple_of(16);
            let count = height.div_ceil(rows);
            let last = height - (count - 1) * rows;
            (count >= 2 && rows >= MIN_BAND_ROWS && last >= MIN_BAND_ROWS).then_some((rows, count))
        })?;
    // Worst case (8x8 MCUs) must fit the 16-bit DRI field.
    if width.div_ceil(8) * (rows / 8) > usize::from(u16::MAX) {
        return None;
    }
    Some((rows, count))
}

/// Encodes `pixels` (`width * height` samples of `bytes_per_pixel`) as bands and splices them.
/// `compress(rows)` returns a ready-to-start encoder for a band of that height; it must produce
/// baseline sequential output with standard (non-optimised) Huffman tables. Any failure,
/// including a panic inside a band, returns `None` so the caller can use the normal path.
pub(crate) fn encode<F>(
    pixels: &[u8],
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    threads: usize,
    compress: F,
) -> Option<Vec<u8>>
where
    F: Fn(usize) -> mozjpeg::Compress + Sync,
{
    let (rows, count) = plan(width, height, threads)?;
    let stride = width * bytes_per_pixel;
    if pixels.len() != stride * height {
        return None;
    }
    let bands: Vec<Option<Vec<u8>>> = (0..count)
        .into_par_iter()
        .map(|index| {
            let start = index * rows;
            let end = if index + 1 == count {
                height
            } else {
                start + rows
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut band = compress(end - start).start_compress(Vec::new()).ok()?;
                band.write_scanlines(&pixels[start * stride..end * stride])
                    .ok()?;
                band.finish().ok()
            }))
            .ok()
            .flatten()
        })
        .collect();
    let bands: Vec<Vec<u8>> = bands.into_iter().collect::<Option<_>>()?;
    splice(&bands, rows, height)
}

struct Parsed {
    /// Offset of the SOS marker (where DRI is inserted).
    sos: usize,
    /// Offset of the first entropy-coded byte.
    data: usize,
    /// Offset of the 16-bit height inside SOF0.
    height_at: usize,
    width: usize,
    height: usize,
    /// Largest horizontal / vertical sampling factor.
    max_h: usize,
    max_v: usize,
}

/// Walks the marker segments of a baseline single-scan JPEG up to its SOS.
fn parse(jpeg: &[u8]) -> Option<Parsed> {
    if jpeg.len() < 4 || jpeg[..2] != [0xff, 0xd8] || jpeg[jpeg.len() - 2..] != [0xff, 0xd9] {
        return None;
    }
    let mut pos = 2;
    let mut sof: Option<(usize, usize, usize, usize, usize)> = None;
    loop {
        if pos + 4 > jpeg.len() || jpeg[pos] != 0xff {
            return None;
        }
        let marker = jpeg[pos + 1];
        let length = usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
        if length < 2 || pos + 2 + length > jpeg.len() {
            return None;
        }
        match marker {
            0xc0 => {
                let body = &jpeg[pos + 4..pos + 2 + length];
                if body.len() < 6 || sof.is_some() {
                    return None;
                }
                let components = usize::from(body[5]);
                if components == 0 || body.len() != 6 + components * 3 {
                    return None;
                }
                let (mut max_h, mut max_v) = (1, 1);
                for component in body[6..].chunks_exact(3) {
                    max_h = max_h.max(usize::from(component[1] >> 4));
                    max_v = max_v.max(usize::from(component[1] & 15));
                }
                let height = usize::from(u16::from_be_bytes([body[1], body[2]]));
                let width = usize::from(u16::from_be_bytes([body[3], body[4]]));
                sof = Some((pos + 5, width, height, max_h, max_v));
            }
            // Progressive / arithmetic / lossless frames, DRI and restarts are not expected.
            0xc1..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcf | 0xdd | 0xd0..=0xd8 | 0xd9 => return None,
            0xda => {
                let (height_at, width, height, max_h, max_v) = sof?;
                return Some(Parsed {
                    sos: pos,
                    data: pos + 2 + length,
                    height_at,
                    width,
                    height,
                    max_h,
                    max_v,
                });
            }
            _ => {}
        }
        pos += 2 + length;
    }
}

/// Entropy-coded bytes must hold only stuffed 0xFF00 pairs: a stray marker (RSTn, a second
/// scan) means the band is not what the splice assumes.
fn is_plain_scan_data(data: &[u8]) -> bool {
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0xff {
            if data.get(i + 1) != Some(&0) {
                return false;
            }
            i += 1;
        }
        i += 1;
    }
    true
}

/// Joins independently encoded bands (all but the last exactly `band_rows` tall) into one
/// JPEG of `total_height` rows with a restart marker between bands. `None` if the bands are
/// inconsistent (different headers or tables, unexpected markers, interval over 65535).
pub(crate) fn splice(bands: &[Vec<u8>], band_rows: usize, total_height: usize) -> Option<Vec<u8>> {
    let first = bands.first()?;
    let head = parse(first)?;
    if bands.len() < 2 || total_height > usize::from(u16::MAX) {
        return None;
    }
    let (mcu_w, mcu_h) = (head.max_h * 8, head.max_v * 8);
    if band_rows == 0 || !band_rows.is_multiple_of(mcu_h) || head.height != band_rows {
        return None;
    }
    let interval = u16::try_from(head.width.div_ceil(mcu_w) * (band_rows / mcu_h)).ok()?;
    let reference = &first[..head.sos];
    let mut size = head.sos + 6 + (head.data - head.sos) + 2;
    let mut datas = Vec::with_capacity(bands.len());
    for (index, band) in bands.iter().enumerate() {
        let parsed = parse(band)?;
        let expected_rows = if index + 1 == bands.len() {
            // The fixed restart interval cannot cover a longer last band.
            total_height
                .checked_sub(band_rows * index)
                .filter(|rows| (1..=band_rows).contains(rows))?
        } else {
            band_rows
        };
        // Same tables and frame header apart from the height; same scan header.
        let mut header = band[..parsed.sos].to_vec();
        header[parsed.height_at..parsed.height_at + 2].copy_from_slice(&head_height(first, &head));
        if parsed.height != expected_rows
            || parsed.width != head.width
            || parsed.height_at != head.height_at
            || header != reference
            || band[parsed.sos..parsed.data] != first[head.sos..head.data]
        {
            return None;
        }
        let data = &band[parsed.data..band.len() - 2];
        if !is_plain_scan_data(data) {
            return None;
        }
        size += data.len() + 2;
        datas.push(data);
    }
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&first[..head.sos]);
    out[head.height_at..head.height_at + 2].copy_from_slice(&(total_height as u16).to_be_bytes());
    out.extend_from_slice(&[0xff, 0xdd, 0, 4]);
    out.extend_from_slice(&interval.to_be_bytes());
    out.extend_from_slice(&first[head.sos..head.data]);
    for (index, data) in datas.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(&[0xff, 0xd0 + ((index - 1) % 8) as u8]);
        }
        out.extend_from_slice(data);
    }
    out.extend_from_slice(&[0xff, 0xd9]);
    Some(out)
}

fn head_height(first: &[u8], head: &Parsed) -> [u8; 2] {
    [first[head.height_at], first[head.height_at + 1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(width: usize, rows: usize, seed: usize, gray: bool, sampling: (u8, u8)) -> Vec<u8> {
        let channels = if gray { 1 } else { 3 };
        let pixels: Vec<u8> = (0..width * rows * channels)
            .map(|i| ((i * 7 + seed * 13 + (i / width) * 3) % 251) as u8)
            .collect();
        let mut compress = mozjpeg::Compress::new(if gray {
            mozjpeg::ColorSpace::JCS_GRAYSCALE
        } else {
            mozjpeg::ColorSpace::JCS_RGB
        });
        compress.set_size(width, rows);
        compress.set_fastest_defaults();
        compress.set_quality(80.0);
        compress.set_optimize_coding(false);
        compress
            .set_chroma_sampling_pixel_sizes((sampling.0, sampling.1), (sampling.0, sampling.1));
        let mut started = compress.start_compress(Vec::new()).unwrap();
        started.write_scanlines(&pixels).unwrap();
        started.finish().unwrap()
    }

    #[test]
    fn plan_respects_limits() {
        assert_eq!(plan(4000, 3000, 1), None);
        assert_eq!(plan(1000, 999, 8), None);
        assert_eq!(plan(1000, 1000, 1), None);
        let (rows, count) = plan(4000, 3000, 12).unwrap();
        assert_eq!(rows % 16, 0);
        assert!(rows >= 64 && (2..=12).contains(&count));
        assert!(3000 - (count - 1) * rows >= 64);
        for height in [1000, 1001, 1087, 4096, 20000] {
            for threads in 2..=16 {
                if let Some((rows, count)) = plan(1200, height, threads) {
                    assert_eq!(rows % 16, 0);
                    let last = height - (count - 1) * rows;
                    assert!(count <= threads && (64..=rows).contains(&last));
                }
            }
        }
    }

    #[test]
    fn splice_builds_one_restart_stream() {
        for (gray, sampling, mcu_h) in [(false, (2, 2), 16), (false, (1, 1), 8), (true, (1, 1), 8)]
        {
            let bands = vec![
                band(100, 64, 1, gray, sampling),
                band(100, 64, 2, gray, sampling),
                band(100, 50, 3, gray, sampling),
            ];
            let joined = splice(&bands, 64, 178).unwrap();
            let dri_at = joined
                .windows(4)
                .position(|w| w == [0xff, 0xdd, 0, 4])
                .unwrap();
            let interval = 100usize.div_ceil(if mcu_h == 16 { 16 } else { 8 }) * (64 / mcu_h);
            assert_eq!(
                joined[dri_at + 4..dri_at + 6],
                (interval as u16).to_be_bytes()
            );
            // Without the DRI segment the stream is a plain single-scan baseline header.
            let mut plain = joined.clone();
            plain.drain(dri_at..dri_at + 6);
            let parsed = parse(&plain).unwrap();
            assert_eq!((parsed.width, parsed.height), (100, 178));
            assert_eq!(parsed.sos, dri_at);
            // Exactly two restart markers, numbered from RST0.
            let rst: Vec<u8> = joined[parsed.data + 6..]
                .windows(2)
                .filter(|w| w[0] == 0xff && (0xd0..=0xd7).contains(&w[1]))
                .map(|w| w[1])
                .collect();
            assert_eq!(rst, [0xd0, 0xd1]);
            assert_eq!(joined[joined.len() - 2..], [0xff, 0xd9]);
        }
    }

    #[test]
    fn splice_rejects_inconsistent_bands() {
        let a = band(100, 64, 1, false, (2, 2));
        let b = band(100, 64, 2, false, (2, 2));
        let other_size = band(96, 64, 3, false, (2, 2));
        let other_sampling = band(100, 64, 3, false, (1, 1));
        assert!(splice(&[a.clone(), b.clone()], 64, 128).is_some());
        assert!(splice(&[a.clone(), other_size], 64, 128).is_none());
        assert!(splice(&[a.clone(), other_sampling], 64, 128).is_none());
        // Wrong total height / band height.
        assert!(splice(&[a.clone(), b.clone()], 64, 100_000).is_none());
        assert!(splice(&[a.clone(), b.clone()], 32, 96).is_none());
        assert!(splice(std::slice::from_ref(&a), 64, 64).is_none());
        // A last band longer than the others would need a restart inside it.
        assert!(splice(&[a.clone(), b.clone()], 64, 129).is_none());
        let mut broken = b;
        broken.truncate(broken.len() - 2);
        assert!(splice(&[a, broken], 64, 128).is_none());
    }
}
