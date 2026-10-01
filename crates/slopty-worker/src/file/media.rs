//! What a file tile shows rather than edits: a picture or a PDF, known by its first bytes.
//!
//! The name says nothing reliable (a screenshot saved without an extension, a `.png` that is
//! really a JPEG), and the client's platform decodes by content anyway, so the worker looks at
//! the signature each format opens with. Only formats Apple's `ImageIO` and `CoreGraphics` draw
//! are named; any other binary stays `FileRead::Binary`. SVG is text and stays editable.

/// Bytes [`sniff`] needs to tell every format it knows.
pub const HEAD: usize = 32;

/// The media type of a file that opens with `head`, when it is a picture or a PDF.
#[must_use]
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| head.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if starts(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some("image/gif");
    }
    if starts(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    if starts(b"%PDF-") {
        return Some("application/pdf");
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return Some("image/tiff");
    }
    if starts(b"\xff\x0a") || starts(b"\0\0\0\x0cJXL \r\n\x87\n") {
        return Some("image/jxl");
    }
    if starts(b"\0\0\x01\0") && head.get(4..6).is_some_and(|n| n != [0, 0]) {
        return Some("image/vnd.microsoft.icon");
    }
    if starts(b"8BPS\0\x01") {
        return Some("image/vnd.adobe.photoshop");
    }
    if let Some(kind) = iso_media(head) {
        return Some(kind);
    }
    bitmap(head).then_some("image/bmp")
}

/// An ISO base media file's picture (HEIF, HEIC, AVIF), by the major brand of its `ftyp` box.
fn iso_media(head: &[u8]) -> Option<&'static str> {
    if head.get(4..8) != Some(b"ftyp") {
        return None;
    }
    match head.get(8..12)? {
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" => Some("image/heic"),
        b"mif1" | b"msf1" => Some("image/heif"),
        b"avif" | b"avis" => Some("image/avif"),
        _ => None,
    }
}

/// A Windows bitmap: `BM`, then a header size only a bitmap has. The two letters alone would
/// take a text that starts with them.
fn bitmap(head: &[u8]) -> bool {
    const HEADERS: [u32; 6] = [12, 40, 52, 56, 108, 124];
    head.starts_with(b"BM")
        && head
            .get(14..18)
            .and_then(|b| b.try_into().ok())
            .is_some_and(|b| HEADERS.contains(&u32::from_le_bytes(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signature_names_its_type_and_text_names_none() {
        let mut bmp = b"BM\x46\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0".to_vec();
        bmp.resize(HEAD, 0);
        let cases: [(&[u8], Option<&str>); 14] = [
            (b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", Some("image/png")),
            (b"\xff\xd8\xff\xe0\0\x10JFIF", Some("image/jpeg")),
            (b"GIF89a\x01\0\x01\0", Some("image/gif")),
            (b"RIFF\x24\0\0\0WEBPVP8 ", Some("image/webp")),
            (b"%PDF-1.7\n%\xe2\xe3", Some("application/pdf")),
            (b"II*\0\x08\0\0\0", Some("image/tiff")),
            (b"\0\0\0\x18ftypheic\0\0\0\0mif1heic", Some("image/heic")),
            (b"\0\0\0\x1cftypavif\0\0\0\0avifmif1", Some("image/avif")),
            (b"\0\0\0\x18ftypmp42\0\0\0\0", None),
            (&bmp, Some("image/bmp")),
            (b"BMW notes: service at 30k\n", None),
            (b"fn main() {}\n", None),
            (b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", None),
            (b"", None),
        ];
        for (head, kind) in cases {
            assert_eq!(sniff(head), kind, "{:?}", String::from_utf8_lossy(head));
        }
    }
}
