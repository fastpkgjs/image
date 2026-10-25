//! Minimal in-place editing of a raw TIFF/EXIF block (the payload after `Exif\0\0`).
//!
//! Only IFD0 entries that already exist are rewritten; nothing is ever resized.

const TAG_X_RESOLUTION: u16 = 0x011a;
const TAG_Y_RESOLUTION: u16 = 0x011b;
const TAG_RESOLUTION_UNIT: u16 = 0x0128;
const TAG_ORIENTATION: u16 = 0x0112;

struct Tiff<'a> {
    data: &'a mut [u8],
    little: bool,
}

impl Tiff<'_> {
    fn u16_at(&self, at: usize) -> Option<u16> {
        let bytes: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let bytes: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn put_u16(&mut self, at: usize, value: u16) -> bool {
        let bytes = if self.little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        match self.data.get_mut(at..at + 2) {
            Some(slot) => {
                slot.copy_from_slice(&bytes);
                true
            }
            None => false,
        }
    }

    fn put_u32(&mut self, at: usize, value: u32) -> bool {
        let bytes = if self.little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        match self.data.get_mut(at..at + 4) {
            Some(slot) => {
                slot.copy_from_slice(&bytes);
                true
            }
            None => false,
        }
    }

    /// Offset of the 12-byte IFD0 entry for `tag`, if present.
    fn find(&self, tag: u16) -> Option<usize> {
        let ifd = self.u32_at(4)? as usize;
        let count = self.u16_at(ifd)? as usize;
        (0..count)
            .map(|i| ifd + 2 + i * 12)
            .find(|&entry| self.u16_at(entry) == Some(tag))
    }
}

fn open(exif: &mut [u8]) -> Option<Tiff<'_>> {
    let little = match exif.get(..4)? {
        b"II*\x00" => true,
        b"MM\x00*" => false,
        _ => return None,
    };
    Some(Tiff { data: exif, little })
}

/// Sets the IFD0 Orientation tag. Returns whether the tag was found and rewritten.
pub fn set_orientation(exif: &mut [u8], orientation: u16) -> bool {
    let Some(mut tiff) = open(exif) else {
        return false;
    };
    let Some(entry) = tiff.find(TAG_ORIENTATION) else {
        return false;
    };
    // type SHORT, count 1: the value lives inline in the entry.
    tiff.u16_at(entry + 2) == Some(3) && tiff.put_u16(entry + 8, orientation)
}

/// Sets XResolution/YResolution to `density` pixels per inch (unit 2). Returns whether any
/// resolution tag was rewritten.
pub fn set_density(exif: &mut [u8], density: u32) -> bool {
    let Some(mut tiff) = open(exif) else {
        return false;
    };
    let mut changed = false;
    for tag in [TAG_X_RESOLUTION, TAG_Y_RESOLUTION] {
        let Some(entry) = tiff.find(tag) else {
            continue;
        };
        // type RATIONAL, count 1: the value field holds the offset of numerator/denominator.
        if tiff.u16_at(entry + 2) != Some(5) {
            continue;
        }
        let Some(at) = tiff.u32_at(entry + 8).map(|v| v as usize) else {
            continue;
        };
        changed |= tiff.put_u32(at, density) && tiff.put_u32(at + 4, 1);
    }
    if let Some(entry) = tiff.find(TAG_RESOLUTION_UNIT) {
        if tiff.u16_at(entry + 2) == Some(3) {
            tiff.put_u16(entry + 8, 2);
        }
    }
    changed
}

/// Smallest valid EXIF block that only carries an Orientation tag.
pub fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"II*\x00");
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&TAG_ORIENTATION.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_orientation_in_both_byte_orders() {
        let mut le = orientation_only(6);
        assert!(set_orientation(&mut le, 1));
        assert_eq!(le, orientation_only(1));

        let mut be = b"MM\x00*\x00\x00\x00\x08\x00\x01\x01\x12\x00\x03\x00\x00\x00\x01\x00\x08\x00\x00\x00\x00\x00\x00"
            .to_vec();
        assert!(set_orientation(&mut be, 3));
        assert_eq!(&be[18..20], &[0, 3]);
    }

    #[test]
    fn ignores_garbage() {
        assert!(!set_orientation(&mut [1, 2, 3], 1));
        assert!(!set_density(&mut b"II*\x00\xff\xff\xff\xff".to_vec(), 72));
    }
}
