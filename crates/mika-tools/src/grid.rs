//! Pictures for pointing: a numbered grid over a screenshot, so a vision model
//! names a cell instead of guessing pixels, and finding words of a label among
//! the words recognised on the screen.

use crate::sys::OcrWord;

/// RGB pixels, top row first.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A box in picture pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn center(self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

pub fn decode(png_bytes: &[u8]) -> Option<Picture> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let data = &buf[..info.buffer_size()];
    let rgb = match info.color_type {
        png::ColorType::Rgb => data.to_vec(),
        png::ColorType::Rgba => data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[r, g, b, _]| [*r, *g, *b])
            .collect(),
        _ => return None,
    };
    Some(Picture {
        rgb,
        width: info.width,
        height: info.height,
    })
}

pub fn encode(p: &Picture) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, p.width, p.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(&p.rgb).ok()?;
    }
    Some(out)
}

/// A part of the picture; the box is clamped to it.
pub fn crop(p: &Picture, a: Area) -> (Picture, Area) {
    let x0 = (a.x.max(0.0) as u32).min(p.width.saturating_sub(1));
    let y0 = (a.y.max(0.0) as u32).min(p.height.saturating_sub(1));
    let x1 = ((a.x + a.w).ceil().max(0.0) as u32).clamp(x0 + 1, p.width);
    let y1 = ((a.y + a.h).ceil().max(0.0) as u32).clamp(y0 + 1, p.height);
    let (w, h) = (x1 - x0, y1 - y0);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in y0..y1 {
        let start = ((y * p.width + x0) * 3) as usize;
        rgb.extend_from_slice(&p.rgb[start..start + (w * 3) as usize]);
    }
    (
        Picture {
            rgb,
            width: w,
            height: h,
        },
        Area {
            x: x0 as f32,
            y: y0 as f32,
            w: w as f32,
            h: h as f32,
        },
    )
}

/// Cell `n` (from 1, row by row) of a `cols` × `rows` grid over `w` × `h`.
pub fn cell(w: u32, h: u32, cols: u32, rows: u32, n: u32) -> Option<Area> {
    if n == 0 || n > cols * rows {
        return None;
    }
    let (cw, ch) = (w as f32 / cols as f32, h as f32 / rows as f32);
    let i = n - 1;
    Some(Area {
        x: (i % cols) as f32 * cw,
        y: (i / cols) as f32 * ch,
        w: cw,
        h: ch,
    })
}

/// The first whole number in a model's answer.
pub fn first_number(answer: &str) -> Option<u32> {
    let digits: String = answer
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Digits 0–9 as 3×5 bitmaps, rows top first, bit 2 is the left column.
const DIGITS: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 2, 2, 2],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

fn put(p: &mut Picture, x: i64, y: i64, c: [u8; 3]) {
    if x < 0 || y < 0 || x >= p.width as i64 || y >= p.height as i64 {
        return;
    }
    let i = ((y as u32 * p.width + x as u32) * 3) as usize;
    p.rgb[i..i + 3].copy_from_slice(&c);
}

fn fill(p: &mut Picture, x: i64, y: i64, w: i64, h: i64, c: [u8; 3]) {
    for yy in y..y + h {
        for xx in x..x + w {
            put(p, xx, yy, c);
        }
    }
}

fn label(p: &mut Picture, x: i64, y: i64, n: u32, scale: i64) {
    let text = n.to_string();
    let wide = text.len() as i64 * 4 * scale + scale;
    fill(p, x, y, wide, 7 * scale, [0, 0, 0]);
    for (k, ch) in text.bytes().enumerate() {
        let glyph = DIGITS[(ch - b'0') as usize];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..3 {
                if bits & (4 >> col) != 0 {
                    fill(
                        p,
                        x + scale + (k as i64 * 4 + col) * scale,
                        y + scale + row as i64 * scale,
                        scale,
                        scale,
                        [255, 230, 0],
                    );
                }
            }
        }
    }
}

/// A crosshair over the point: two lines in a dark outline, a gap in the middle
/// so the thing under it stays visible.
pub fn crosshair(p: &Picture, x: i64, y: i64) -> Picture {
    let mut out = p.clone();
    let (arm, gap) = (16, 5);
    for (dx, dy, w, h) in [
        (-arm - gap, -1, arm, 3),
        (gap, -1, arm, 3),
        (-1, -arm - gap, 3, arm),
        (-1, gap, 3, arm),
    ] {
        fill(&mut out, x + dx - 1, y + dy - 1, w + 2, h + 2, [0, 0, 0]);
        fill(&mut out, x + dx, y + dy, w, h, [255, 30, 30]);
    }
    out
}

/// The first two whole numbers of an answer: `x=120, y=340` → (120, 340).
pub fn two_numbers(answer: &str) -> Option<(u32, u32)> {
    let mut nums = answer
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok());
    Some((nums.next()?, nums.next()?))
}

/// A yes or no from a model's answer; `None` when it is neither.
pub fn yes(answer: &str) -> Option<bool> {
    let a = answer.trim().to_lowercase();
    let first: String = a.chars().take_while(|c| c.is_alphabetic()).collect();
    match first.as_str() {
        "да" | "yes" => Some(true),
        "нет" | "no" => Some(false),
        _ => None,
    }
}

/// Draws grid lines and the number of each cell in its top left corner.
pub fn draw_grid(p: &Picture, cols: u32, rows: u32) -> Picture {
    let mut out = p.clone();
    let (w, h) = (p.width as i64, p.height as i64);
    let ch = h / rows as i64;
    let scale = (ch / 30).clamp(2, 6);
    for c in 1..cols {
        let x = c as i64 * w / cols as i64;
        fill(&mut out, x - 1, 0, 3, h, [0, 0, 0]);
        fill(&mut out, x, 0, 1, h, [255, 0, 200]);
    }
    for r in 1..rows {
        let y = r as i64 * h / rows as i64;
        fill(&mut out, 0, y - 1, w, 3, [0, 0, 0]);
        fill(&mut out, 0, y, w, 1, [255, 0, 200]);
    }
    for n in 1..=cols * rows {
        let i = (n - 1) as i64;
        let x = (i % cols as i64) * w / cols as i64;
        let y = (i / cols as i64) * h / rows as i64;
        label(&mut out, x + 2, y + 2, n, scale);
    }
    out
}

fn plain(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Boxes of the label among the recognised words: its words in a row on one
/// line. A recognised word may carry punctuation or an ending («Поиск…», «Сохранить»).
pub fn find_label(words: &[OcrWord], target: &str) -> Vec<Area> {
    let want: Vec<String> = target
        .split_whitespace()
        .map(plain)
        .filter(|w| !w.is_empty())
        .collect();
    if want.is_empty() {
        return Vec::new();
    }
    let fits = |got: &str, want: &str| {
        let got = plain(got);
        !got.is_empty() && (got == want || (want.chars().count() >= 3 && got.starts_with(want)))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i + want.len() <= words.len() {
        let run = &words[i..i + want.len()];
        let one_line = run.iter().all(|w| w.line == run[0].line);
        if one_line && run.iter().zip(&want).all(|(g, w)| fits(&g.text, w)) {
            let x0 = run.iter().map(|w| w.x).fold(f32::MAX, f32::min);
            let y0 = run.iter().map(|w| w.y).fold(f32::MAX, f32::min);
            let x1 = run.iter().map(|w| w.x + w.w).fold(0.0, f32::max);
            let y1 = run.iter().map(|w| w.y + w.h).fold(0.0, f32::max);
            out.push(Area {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            });
            i += want.len();
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, line: u32, x: f32) -> OcrWord {
        OcrWord {
            text: text.into(),
            line,
            x,
            y: 10.0 * line as f32,
            w: 20.0,
            h: 8.0,
        }
    }

    #[test]
    fn a_label_is_found_word_by_word_on_one_line() {
        let words = vec![
            word("Файл", 0, 0.0),
            word("Сохранить", 0, 30.0),
            word("как…", 0, 60.0),
            word("Сохранить", 1, 0.0),
        ];
        let found = find_label(&words, "сохранить как");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].x, found[0].w), (30.0, 50.0));
        assert_eq!(find_label(&words, "Сохранить").len(), 2);
        assert!(find_label(&words, "Открыть").is_empty());
        // Two letters are too short to stand for a longer word.
        assert!(find_label(&words, "Фа").is_empty());
    }

    #[test]
    fn cells_count_row_by_row_and_numbers_are_read() {
        let a = cell(800, 600, 8, 6, 10).unwrap();
        assert_eq!((a.x, a.y, a.w, a.h), (100.0, 100.0, 100.0, 100.0));
        assert!(cell(800, 600, 8, 6, 49).is_none() && cell(800, 600, 8, 6, 0).is_none());
        assert_eq!(first_number("Клетка 17."), Some(17));
        assert_eq!(first_number("нет"), None);
    }

    #[test]
    fn grid_and_crop_keep_the_picture_valid() {
        let p = Picture {
            rgb: vec![128; 400 * 300 * 3],
            width: 400,
            height: 300,
        };
        let g = draw_grid(&p, 8, 6);
        assert_eq!(g.rgb.len(), p.rgb.len());
        assert_ne!(g.rgb, p.rgb);
        let png_bytes = encode(&g).unwrap();
        assert_eq!(decode(&png_bytes).unwrap(), g);
        let (part, at) = crop(
            &g,
            Area {
                x: -10.0,
                y: 250.0,
                w: 100.0,
                h: 100.0,
            },
        );
        assert_eq!((part.width, part.height), (90, 50));
        assert_eq!((at.x, at.y), (0.0, 250.0));
    }
}
