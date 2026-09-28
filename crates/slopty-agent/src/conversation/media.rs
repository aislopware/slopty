//! Pictures in a transcript: a prompt's pasted image, a screenshot a tool returned, a picture a
//! `Read` opened.
//!
//! Claude Code keeps each one base64 in its record: an `image` content block
//! (`{"type":"image","source":{"type":"base64","media_type":…,"data":…}}`), in a prompt's
//! content or in a tool result's, and for a `Read` of an image also in
//! `toolUseResult.file` (`{"base64":…,"type":"image/png"}`). The decoder describes each one
//! ([`describe`]): its digest, type, size in bytes and in pixels, and where it is. The bytes
//! stay in the transcript until a client asks for them ([`bytes_at`]).

use data_encoding::BASE64;
use serde_json::Value;

use super::{Image, Part, TextRef, str_at};

/// A picture's base64 and its media type, if `block` is one: an image content block, or a
/// `Read` result's `file`.
fn encoded(block: &Value) -> Option<(&str, Option<&str>)> {
    if str_at(block, "type") == Some("image")
        && let Some(source) = block.get("source")
        && str_at(source, "type") == Some("base64")
    {
        return Some((str_at(source, "data")?, str_at(source, "media_type")));
    }
    Some((str_at(block, "base64")?, str_at(block, "type")))
}

/// The bytes of base64 as Claude Code writes it (standard, padded); `None` when it is not.
fn decode(data: &str) -> Option<Vec<u8>> {
    BASE64.decode(data.trim().as_bytes()).ok()
}

/// A picture's description, when `block` holds one; `at` is where it is.
#[must_use]
pub fn describe(block: &Value, at: TextRef) -> Option<Image> {
    let (data, media_type) = encoded(block)?;
    let bytes = decode(data)?;
    let (width, height) = dimensions(&bytes).unwrap_or((0, 0));
    let media_type = media_type
        .filter(|t| t.starts_with("image/"))
        .or_else(|| sniff(&bytes))
        .unwrap_or("image/png")
        .to_owned();
    Some(Image {
        digest: blake3::hash(&bytes).to_hex().to_string(),
        media_type,
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        width,
        height,
        at,
    })
}

/// The pictures of a prompt's content blocks, each at its block.
#[must_use]
pub fn in_prompt(record: &str, blocks: &[&Value]) -> Vec<Image> {
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| str_at(b, "type") == Some("image"))
        .filter_map(|(index, block)| {
            let part = Part::Image { tool_use_id: None, index: super::to_u32(index) };
            describe(block, TextRef { record: record.to_owned(), part })
        })
        .collect()
}

/// The pictures of a tool result: its image blocks, else the picture `toolUseResult` holds
/// (a `Read` of an image).
#[must_use]
pub fn in_result(record: &str, id: &str, block: &Value, result: Option<&Value>) -> Vec<Image> {
    let at = |index: usize| TextRef {
        record: record.to_owned(),
        part: Part::Image { tool_use_id: Some(id.to_owned()), index: super::to_u32(index) },
    };
    let blocks = block.get("content").and_then(Value::as_array);
    let found: Vec<Image> = blocks
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, b)| str_at(b, "type") == Some("image"))
        .filter_map(|(index, b)| describe(b, at(index)))
        .collect();
    if !found.is_empty() {
        return found;
    }
    result
        .filter(|r| str_at(r, "type") == Some("image"))
        .and_then(|r| r.get("file"))
        .and_then(|file| describe(file, at(0)))
        .into_iter()
        .collect()
}

/// The bytes of the picture `part` names in `record`; `None` when it has none there.
#[must_use]
pub fn bytes_at(record: &Value, part: &Part) -> Option<Vec<u8>> {
    let Part::Image { tool_use_id, index } = part else { return None };
    let index = usize::try_from(*index).ok()?;
    let content = record.get("message")?.get("content")?.as_array()?;
    let block = match tool_use_id {
        None => content.get(index)?,
        Some(id) => {
            let result = content.iter().find(|b| str_at(b, "tool_use_id") == Some(id.as_str()))?;
            match result.get("content").and_then(Value::as_array).and_then(|c| c.get(index)) {
                Some(block) if str_at(block, "type") == Some("image") => block,
                _ => record.get("toolUseResult")?.get("file")?,
            }
        }
    };
    decode(encoded(block)?.0)
}

/// The media type a picture's first bytes say.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    Some(u32::from(u16::from_be_bytes([*b.first()?, *b.get(1)?])))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    Some(u32::from(u16::from_le_bytes([*b.first()?, *b.get(1)?])))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(3)?)?;
    Some(u32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, 0]))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
}

/// A picture's width and height in pixels, from the header of the four formats the model
/// reads (PNG, JPEG, GIF, WebP); `None` for anything else or a header cut short.
#[must_use]
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match sniff(bytes)? {
        "image/png" => Some((be32(bytes, 16)?, be32(bytes, 20)?)),
        "image/gif" => Some((le16(bytes, 6)?, le16(bytes, 8)?)),
        "image/webp" => match bytes.get(12..16)? {
            b"VP8 " => Some((le16(bytes, 26)? & 0x3fff, le16(bytes, 28)? & 0x3fff)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff).checked_add(1)?, ((bits >> 14) & 0x3fff).checked_add(1)?))
            }
            b"VP8X" => Some((le24(bytes, 24)?.checked_add(1)?, le24(bytes, 27)?.checked_add(1)?)),
            _ => None,
        },
        _ => jpeg_dimensions(bytes),
    }
}

/// A JPEG's size from its first start-of-frame marker, walking the segments before it.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2_usize;
    loop {
        if *bytes.get(at)? != 0xff {
            return None;
        }
        let marker = *bytes.get(at.checked_add(1)?)?;
        match marker {
            // Fill bytes before a marker.
            0xff => at = at.checked_add(1)?,
            // Markers with no length: the restarts, start of image, the temporary one.
            0x01 | 0xd0..=0xd8 => at = at.checked_add(2)?,
            // Every start-of-frame but the Huffman table, the extension and the arithmetic one.
            0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                let height = be16(bytes, at.checked_add(5)?)?;
                let width = be16(bytes, at.checked_add(7)?)?;
                return Some((width, height));
            }
            _ => {
                let length = usize::try_from(be16(bytes, at.checked_add(2)?)?).ok()?;
                at = at.checked_add(2)?.checked_add(length)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A 3 × 2 PNG's header, as a picture would begin.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0]);
        bytes
    }

    /// Each format's header gives its size; a JPEG's is found past the segments before it.
    #[test]
    fn a_pictures_header_gives_its_size() {
        assert_eq!(dimensions(&png(1_600, 900)), Some((1_600, 900)));
        let gif = [b"GIF89a".as_slice(), &[0x40, 0x01, 0xf0, 0x00]].concat();
        assert_eq!(dimensions(&gif), Some((320, 240)));
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend([0xff, 0xc0, 0x00, 0x11, 0x08, 0x02, 0x58, 0x03, 0x20]);
        assert_eq!(dimensions(&jpeg), Some((800, 600)));
        let mut vp8x = b"RIFF\x00\x00\x00\x00WEBPVP8X".to_vec();
        vp8x.extend([0; 8]);
        vp8x.extend([0x7f, 0x02, 0x00, 0xdf, 0x01, 0x00]);
        assert_eq!(dimensions(&vp8x), Some((640, 480)));
        assert_eq!(dimensions(b"not a picture"), None);
        assert_eq!(dimensions(&png(1, 1)[..12]), None, "a header cut short");
    }

    /// A pasted picture is described by the digest of its bytes, its type and its size, and
    /// its bytes come back from where it is.
    #[test]
    fn a_picture_is_described_and_found_again() {
        let bytes = png(3, 2);
        let data = BASE64.encode(&bytes);
        let block = json!({
            "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": data },
        });
        let record =
            json!({ "message": { "content": [{ "type": "text", "text": "look" }, block] } });
        let blocks: Vec<&Value> = record["message"]["content"].as_array().unwrap().iter().collect();
        let [image] = in_prompt("u1", &blocks).try_into().unwrap();
        assert_eq!(image.digest, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!((image.width, image.height, image.bytes), (3, 2, 29));
        assert_eq!(image.at.part, Part::Image { tool_use_id: None, index: 1 });
        assert_eq!(bytes_at(&record, &image.at.part), Some(bytes));
        assert_eq!(bytes_at(&record, &Part::Image { tool_use_id: None, index: 0 }), None);
    }

    /// A `Read` of a picture keeps it in its result's content and in `toolUseResult.file`;
    /// either way it is block 0 of the result.
    #[test]
    fn a_read_picture_is_found_in_its_result() {
        let bytes = png(10, 20);
        let data = BASE64.encode(&bytes);
        let file = json!({ "type": "image", "file": { "base64": data, "type": "image/png" } });
        let bare = json!({ "type": "tool_result", "tool_use_id": "t1", "content": [] });
        let [image] = in_result("r1", "t1", &bare, Some(&file)).try_into().unwrap();
        assert_eq!((image.width, image.height), (10, 20));
        let record = json!({
            "message": { "content": [bare] }, "toolUseResult": file,
        });
        assert_eq!(bytes_at(&record, &image.at.part), Some(bytes.clone()));
        let blocks = json!({
            "type": "tool_result", "tool_use_id": "t1",
            "content": [{ "type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": data,
            }}],
        });
        let [image] = in_result("r1", "t1", &blocks, None).try_into().unwrap();
        assert_eq!(image.at.part, Part::Image { tool_use_id: Some("t1".to_owned()), index: 0 });
        let record = json!({ "message": { "content": [blocks] } });
        assert_eq!(bytes_at(&record, &image.at.part), Some(bytes));
    }
}
