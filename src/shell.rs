//! Start view and the left strip. No browser and no image decoder.

pub const STRIP_COLLAPSED: i32 = 48;
pub const STRIP_EXPANDED: i32 = 220;
const ROW: i32 = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Start,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Start,
    Browser,
}

pub fn strip_width(expanded: bool) -> i32 {
    if expanded {
        STRIP_EXPANDED
    } else {
        STRIP_COLLAPSED
    }
}

pub fn hit_strip(expanded: bool, x: f64, y: f64) -> Option<Entry> {
    if x < 0.0 || y < 0.0 || x >= f64::from(strip_width(expanded)) {
        return None;
    }
    let row = (y / f64::from(ROW)) as i32;
    match row {
        0 => Some(Entry::Start),
        1 => Some(Entry::Browser),
        _ => None,
    }
}

/// XRGB8888, little-endian memory order B, G, R, X.
pub fn fill_gradient(width: i32, height: i32) -> Vec<u8> {
    let mut buf = vec![0u8; (width * height * 4) as usize];
    paint_gradient(&mut buf, width, height);
    buf
}

pub fn paint_gradient(buf: &mut [u8], width: i32, height: i32) {
    let top = [28u8, 36, 48];
    let bottom = [62u8, 72, 58];
    let last = (height - 1).max(1);
    for y in 0..height {
        let t = y as u32;
        let r = blend(top[0], bottom[0], t, last as u32);
        let g = blend(top[1], bottom[1], t, last as u32);
        let b = blend(top[2], bottom[2], t, last as u32);
        for x in 0..width {
            put(buf, width, x, y, r, g, b);
        }
    }
}

pub fn paint_strip(width: i32, height: i32, expanded: bool, page: Page) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            put(&mut pixels, width, x, y, 22, 24, 29);
        }
    }
    for (index, entry) in [Entry::Start, Entry::Browser].into_iter().enumerate() {
        let row_y = index as i32 * ROW;
        if row_y >= height {
            break;
        }
        let active = match (page, entry) {
            (Page::Start, Entry::Start) | (Page::Browser, Entry::Browser) => true,
            _ => false,
        };
        if active {
            fill_rect(&mut pixels, width, height, 0, row_y, 3, ROW, 215, 196, 163);
        }
        let (ir, ig, ib) = match entry {
            Entry::Start => (196, 165, 116),
            Entry::Browser => (127, 147, 168),
        };
        fill_rect(&mut pixels, width, height, 10, row_y + 10, 28, 28, ir, ig, ib);
        if entry == Entry::Browser {
            fill_rect(&mut pixels, width, height, 14, row_y + 16, 20, 16, 232, 228, 216);
        }
        if expanded {
            let label = match entry {
                Entry::Start => "Start",
                Entry::Browser => "Browser",
            };
            draw_text(&mut pixels, width, height, 48, row_y + 16, label);
        }
    }
    pixels
}

fn draw_text(buf: &mut [u8], width: i32, height: i32, x: i32, y: i32, text: &str) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = glyph(ch);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    put(buf, width, cursor + col, y + row as i32, 230, 226, 216);
                }
            }
        }
        cursor += 6;
        if cursor >= width || y >= height {
            break;
        }
    }
}

fn glyph(ch: char) -> [u8; 7] {
    match ch {
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'a' => [0b00000, 0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110],
        'e' => [0b00000, 0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b00100, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'w' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        _ => [0; 7],
    }
}

fn fill_rect(
    buf: &mut [u8],
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: u8,
    g: u8,
    b: u8,
) {
    for py in y..(y + h).min(height) {
        if py < 0 {
            continue;
        }
        for px in x..(x + w).min(width) {
            if px >= 0 {
                put(buf, width, px, py, r, g, b);
            }
        }
    }
}

fn put(buf: &mut [u8], width: i32, x: i32, y: i32, r: u8, g: u8, b: u8) {
    if x < 0 || y < 0 || x >= width {
        return;
    }
    let i = ((y * width + x) * 4) as usize;
    if i + 3 >= buf.len() {
        return;
    }
    buf[i] = b;
    buf[i + 1] = g;
    buf[i + 2] = r;
    buf[i + 3] = 255;
}

fn blend(from: u8, to: u8, y: u32, last: u32) -> u8 {
    let v = u32::from(from) * (last - y) + u32::from(to) * y;
    (v / last) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_changes_from_top_to_bottom() {
        let width = 4;
        let height = 8;
        let buf = fill_gradient(width, height);
        assert_eq!(&buf[0..3], &[48, 36, 28]);
        let last = ((height - 1) * width * 4) as usize;
        assert_eq!(&buf[last..last + 3], &[58, 72, 62]);
        assert_ne!(&buf[0..3], &buf[last..last + 3]);
    }

    #[test]
    fn collapsed_strip_ignores_clicks_past_the_icons() {
        assert_eq!(hit_strip(false, 10.0, 10.0), Some(Entry::Start));
        assert_eq!(hit_strip(false, 10.0, 60.0), Some(Entry::Browser));
        assert_eq!(hit_strip(false, 49.0, 10.0), None);
        assert_eq!(hit_strip(true, 180.0, 60.0), Some(Entry::Browser));
    }

    #[test]
    fn strip_buffer_covers_the_panel() {
        let buf = paint_strip(STRIP_COLLAPSED, 100, false, Page::Start);
        assert_eq!(buf.len(), (STRIP_COLLAPSED * 100 * 4) as usize);
    }
}
