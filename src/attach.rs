//! Attachments for a message: images (pasted or picked) and text files,
//! turned into ACP content blocks. The agent reports what it accepts in
//! `promptCapabilities` (Claude takes images and embedded files).

use std::path::Path;

use anyhow::{Result, bail};

use crate::agent::{Attachment, AttachmentKind};

/// The API refuses larger images.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Bigger text files are better read by the agent itself (give it the path).
const MAX_TEXT_BYTES: usize = 256 * 1024;

/// An image from the clipboard.
pub fn from_image(image: &gpui::Image, number: usize) -> Result<Attachment> {
    let (mime, ext) = match image.format {
        gpui::ImageFormat::Png => ("image/png", "png"),
        gpui::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        gpui::ImageFormat::Gif => ("image/gif", "gif"),
        gpui::ImageFormat::Webp => ("image/webp", "webp"),
        _ => bail!("Claude takes PNG, JPEG, GIF and WebP images."),
    };
    if image.bytes.len() > MAX_IMAGE_BYTES {
        bail!("That image is over 5 MB; Claude can't take it. Try a smaller screenshot.");
    }
    Ok(Attachment {
        name: format!("pasted-image-{number}.{ext}"),
        kind: AttachmentKind::Image { mime: mime.into(), data: base64(&image.bytes) },
        preview: Some(std::sync::Arc::new(image.clone())),
    })
}

/// A file you dropped on the message box or picked.
pub fn from_path(path: &Path) -> Result<Attachment> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    let bytes = std::fs::read(path).map_err(|e| anyhow::anyhow!("Couldn't read {name}: {e}"))?;
    if let Some(mime) = mime {
        if bytes.len() > MAX_IMAGE_BYTES {
            bail!("{name} is over 5 MB; Claude can't take images that large.");
        }
        let format = match mime {
            "image/jpeg" => gpui::ImageFormat::Jpeg,
            "image/gif" => gpui::ImageFormat::Gif,
            "image/webp" => gpui::ImageFormat::Webp,
            _ => gpui::ImageFormat::Png,
        };
        let data = base64(&bytes);
        return Ok(Attachment { name, kind: AttachmentKind::Image { mime: mime.into(), data }, preview: Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes))) });
    }
    if bytes.len() > MAX_TEXT_BYTES {
        bail!("{name} is over 256 KB. Mention its path instead and the agent reads what it needs.");
    }
    let Ok(text) = String::from_utf8(bytes) else {
        bail!("{name} isn't text or a PNG, JPEG, GIF or WebP image.");
    };
    let uri = format!("file:///{}", path.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
    Ok(Attachment { name, kind: AttachmentKind::Text { uri, text }, preview: None })
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
