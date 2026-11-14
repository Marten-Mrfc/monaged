//! Images in conversations: thumbnails of what you attached (also after a
//! session reopens, from its history), images an agent shows, and a viewer
//! that opens one full size (Esc or a click closes it).

use std::sync::Arc;

use gpui::{AnyElement, Context, Image, ImageFormat, ObjectFit, SharedString, div, img, prelude::*, px, rgb, rgba};

use crate::{Monaged, theme, ui};

/// An image from ACP content (`{"type":"image","mimeType":…,"data":…}`).
pub fn from_content(content: &serde_json::Value) -> Option<Arc<Image>> {
    let data = content["data"].as_str()?;
    let format = match content["mimeType"].as_str().unwrap_or("image/png") {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Png,
    };
    let bytes = base64_decode(data)?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

impl Monaged {
    /// A clickable thumbnail; opens the viewer.
    pub(crate) fn thumbnail(&self, id: impl Into<SharedString>, image: Arc<Image>, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let full = image.clone();
        div()
            .id(gpui::ElementId::Name(id.into()))
            .size(px(size))
            .flex_none()
            .rounded(px(6.))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::code_bg()))
            .cursor_pointer()
            .hover(|s| s.border_color(rgb(theme::text_3())))
            .child(img(image).size_full().object_fit(ObjectFit::Cover))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.lightbox = Some(full.clone());
                cx.notify();
            }))
            .into_any_element()
    }

    /// The full-size viewer over everything.
    pub(crate) fn render_lightbox(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let image = self.lightbox.clone()?;
        Some(
            div()
                .id("lightbox")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(px(40.))
                .bg(rgba(0x000000d0))
                .occlude()
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.lightbox = None;
                    cx.notify();
                }))
                .child(img(image).max_w(gpui::relative(1.)).max_h(gpui::relative(1.)).size_full().object_fit(ObjectFit::Contain))
                .child(div().absolute().bottom(px(14.)).left_0().right_0().flex().justify_center().child(ui::meta("Click or press Esc to close")))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trips() {
        for sample in [&b""[..], b"f", b"fo", b"foo", b"monaged images"] {
            let encoded = crate::attach::base64(sample);
            assert_eq!(super::base64_decode(&encoded).unwrap(), sample);
        }
    }
}
