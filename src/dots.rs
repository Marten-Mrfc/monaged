//! Dot-matrix numerals, after Nothing OS: for the few numbers that matter
//! (usage, counts, scores). A 5×7 grid per glyph, painted as round dots on a
//! single canvas.

use gpui::{Hsla, IntoElement, Styled, Window, canvas, fill, point, px, size};

/// 5×7 glyphs; '1' is a lit dot.
fn glyph(c: char) -> Option<[&'static str; 7]> {
    Some(match c {
        '0' => ["01110", "10001", "10001", "10001", "10001", "10001", "01110"],
        '1' => ["00100", "01100", "00100", "00100", "00100", "00100", "01110"],
        '2' => ["01110", "10001", "00001", "00010", "00100", "01000", "11111"],
        '3' => ["11110", "00001", "00001", "01110", "00001", "00001", "11110"],
        '4' => ["00010", "00110", "01010", "10010", "11111", "00010", "00010"],
        '5' => ["11111", "10000", "11110", "00001", "00001", "10001", "01110"],
        '6' => ["00110", "01000", "10000", "11110", "10001", "10001", "01110"],
        '7' => ["11111", "00001", "00010", "00100", "01000", "01000", "01000"],
        '8' => ["01110", "10001", "10001", "01110", "10001", "10001", "01110"],
        '9' => ["01110", "10001", "10001", "01111", "00001", "00010", "01100"],
        '%' => ["11001", "11010", "00010", "00100", "01000", "01011", "10011"],
        '$' => ["00100", "01111", "10100", "01110", "00101", "11110", "00100"],
        '.' => ["000", "000", "000", "000", "000", "011", "011"],
        ',' => ["000", "000", "000", "000", "011", "001", "010"],
        '/' => ["00001", "00010", "00010", "00100", "01000", "01000", "10000"],
        '-' => ["0000", "0000", "0000", "1111", "0000", "0000", "0000"],
        ':' => ["0", "0", "1", "0", "1", "0", "0"],
        ' ' => ["00", "00", "00", "00", "00", "00", "00"],
        // Monaged's mark.
        'M' => ["10001", "11011", "10101", "10101", "10001", "10001", "10001"],
        _ => return None,
    })
}

/// The width and height `dot_text` will take, in pixels.
pub fn measure(text: &str, dot: f32) -> (f32, f32) {
    let step = dot * 1.6;
    let columns: usize = text.chars().filter_map(glyph).map(|g| g[0].len() + 1).sum();
    ((columns.saturating_sub(1)) as f32 * step, 7.0 * step - (step - dot))
}

/// Draws `text` as dots of `dot` px. Unknown characters are skipped.
pub fn dot_text(text: &str, dot: f32, color: impl Into<Hsla>) -> impl IntoElement {
    let color = color.into();
    let glyphs: Vec<[&'static str; 7]> = text.chars().filter_map(glyph).collect();
    let (width, height) = measure(text, dot);
    canvas(
        |_, _, _| (),
        move |bounds, _, window: &mut Window, _| {
            let step = dot * 1.6;
            let mut x = 0.0;
            for g in &glyphs {
                for (row, bits) in g.iter().enumerate() {
                    for (col, bit) in bits.bytes().enumerate() {
                        if bit == b'1' {
                            let origin = point(bounds.left() + px(x + col as f32 * step), bounds.top() + px(row as f32 * step));
                            window.paint_quad(fill(gpui::Bounds::new(origin, size(px(dot), px(dot))), color).corner_radii(px(dot / 2.0)));
                        }
                    }
                }
                x += (g[0].len() + 1) as f32 * step;
            }
        },
    )
    .w(px(width))
    .h(px(height))
}
