//! The pixel size of a surface image, a PNG or a JPEG, read from its
//! headers alone: no decoding, no dependency.

/// The image formats a `PixelSurface` carries, told apart by signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// A PNG: signature, then the `IHDR` chunk.
    Png,
    /// A baseline, extended or progressive JPEG.
    Jpeg,
}

impl ImageFormat {
    /// The short name: `png` or `jpeg`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }

    /// The media type: `image/png` or `image/jpeg`.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// The format, width and height of a PNG or JPEG, or why `bytes` is
/// neither or names no area.
///
/// # Errors
///
/// A message naming what is missing, unsupported or zero.
pub fn image_size(bytes: &[u8]) -> Result<(ImageFormat, u32, u32), String> {
    let (format, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height) = png(bytes)?;
        (ImageFormat::Png, width, height)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        let (width, height) = jpeg(bytes)?;
        (ImageFormat::Jpeg, width, height)
    } else {
        return Err("the image is neither a PNG nor a JPEG".to_owned());
    };
    if width == 0 || height == 0 {
        return Err(format!(
            "the {} is {width} x {height}; a surface has area",
            format.name()
        ));
    }
    Ok((format, width, height))
}

fn png(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err("the PNG has no IHDR chunk".to_owned());
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Ok((be(16), be(20)))
}

/// Walk the JPEG's marker segments from `SOI` to its first frame header.
fn jpeg(bytes: &[u8]) -> Result<(u32, u32), String> {
    let truncated = || "the JPEG ends before its frame header".to_owned();
    let mut at = 2;
    loop {
        if *bytes.get(at).ok_or_else(truncated)? != 0xff {
            return Err(format!("the JPEG has no marker at byte {at}"));
        }
        // Any number of 0xff fill bytes may precede a marker.
        while bytes.get(at) == Some(&0xff) {
            at += 1;
        }
        let marker = *bytes.get(at).ok_or_else(truncated)?;
        at += 1;
        // Standalone markers carry no length.
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = bytes.get(at..at + 2).ok_or_else(truncated)?;
        let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        if length < 2 {
            return Err(format!("the JPEG has a {length}-byte segment"));
        }
        match marker {
            // Baseline, extended sequential, progressive (Huffman).
            0xc0..=0xc2 => {
                let header = bytes.get(at + 3..at + 7).ok_or_else(truncated)?;
                let height = u32::from(u16::from_be_bytes([header[0], header[1]]));
                let width = u32::from(u16::from_be_bytes([header[2], header[3]]));
                return Ok((width, height));
            }
            // Every other frame type: lossless, hierarchical, arithmetic.
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(format!(
                    "the JPEG's frame type 0x{marker:02x} is not baseline or progressive"
                ));
            }
            0xd8..=0xda => return Err("the JPEG has no frame header".to_owned()),
            _ => at += length,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASELINE: &[u8] = include_bytes!("../testdata/baseline-5x3.jpg");
    const PROGRESSIVE: &[u8] = include_bytes!("../testdata/progressive-5x3.jpg");

    #[test]
    fn a_baseline_and_a_progressive_jpeg_name_their_size() {
        assert_eq!(image_size(BASELINE), Ok((ImageFormat::Jpeg, 5, 3)));
        assert_eq!(image_size(PROGRESSIVE), Ok((ImageFormat::Jpeg, 5, 3)));
        // Fill bytes and a restart marker before the frame header.
        let mut padded = vec![0xff, 0xd8, 0xff, 0xff, 0xd0];
        padded.extend_from_slice(&[0xff, 0xc2, 0, 11, 8, 0, 3, 0, 5, 1, 1, 0x11, 0]);
        assert_eq!(image_size(&padded), Ok((ImageFormat::Jpeg, 5, 3)));
    }

    #[test]
    fn a_truncated_jpeg_is_refused_at_every_cut() {
        let frame = BASELINE
            .windows(2)
            .position(|pair| pair == [0xff, 0xc0])
            .unwrap();
        for cut in 0..frame + 9 {
            assert!(image_size(&BASELINE[..cut]).is_err(), "cut at {cut}");
        }
        assert!(image_size(&BASELINE[..frame + 9]).is_ok());
    }

    #[test]
    fn garbage_and_unsupported_jpegs_are_refused() {
        assert!(image_size(b"").is_err());
        assert!(image_size(b"GIF89a....").is_err());
        assert!(image_size(&[0xff, 0xd8, 0x00, 0x01]).is_err());
        assert!(image_size(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x01]).is_err());
        // Scan data before any frame header.
        assert!(image_size(&[0xff, 0xd8, 0xff, 0xda, 0, 2]).is_err());
        // Lossless and arithmetic frames.
        for marker in [0xc3, 0xc9] {
            let lossless = [0xff, 0xd8, 0xff, marker, 0, 11, 8, 0, 3, 0, 5, 1];
            assert!(image_size(&lossless).is_err());
        }
        // No area: a height defined later by DNL.
        let flat = [0xff, 0xd8, 0xff, 0xc0, 0, 11, 8, 0, 0, 0, 5, 1];
        assert!(image_size(&flat).is_err());
        let mut fuzz = 0x2545_f491_u32;
        for _ in 0..2000 {
            let mut bytes = vec![0xff, 0xd8];
            for _ in 0..(fuzz % 64) {
                fuzz ^= fuzz << 13;
                fuzz ^= fuzz >> 17;
                fuzz ^= fuzz << 5;
                bytes.push(fuzz.to_le_bytes()[0]);
            }
            let _ = image_size(&bytes);
        }
    }

    #[test]
    fn a_png_names_its_size_and_needs_area() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_size(&png), Ok((ImageFormat::Png, 640, 480)));
        png[20..24].copy_from_slice(&0u32.to_be_bytes());
        assert!(image_size(&png).is_err());
        assert!(image_size(&png[..20]).is_err());
    }
}
