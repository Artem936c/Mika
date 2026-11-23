//! A picture of any kind Windows reads (PNG, JPEG, WebP, GIF, BMP) made into
//! PNG, smaller on the way (decision 0053). Uses the decoders of Windows: no
//! library of our own. A GIF gives its first frame.

use windows::core::{Interface, HSTRING};
use windows::Foundation::IReference;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode,
};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::Com::COINIT_MULTITHREADED;

/// The EXIF property of how a photo was turned.
const ORIENTATION: &str = "System.Photo.Orientation";

/// A picture as PNG with its size.
#[derive(Debug)]
pub struct Png {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decodes `bytes` and gives PNG whose long side is at most `max_side`. A
/// photo turned by its EXIF mark comes out upright.
pub fn to_png(bytes: Vec<u8>, max_side: u32) -> Result<Png, String> {
    super::with_com(COINIT_MULTITHREADED, move || {
        decode(&bytes, max_side).map_err(|e| format!("картинка не прочитана: {}", e.message()))?
    })
}

/// The size that fits `max_side` on the long side, the shape kept. A
/// smaller picture keeps its size.
pub fn scaled(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max_side || long == 0 {
        return (width.max(1), height.max(1));
    }
    let f = max_side as f64 / long as f64;
    (
        ((width as f64 * f).round() as u32).clamp(1, max_side),
        ((height as f64 * f).round() as u32).clamp(1, max_side),
    )
}

fn decode(bytes: &[u8], max_side: u32) -> windows::core::Result<Result<Png, String>> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let (width, height) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
    if width == 0 || height == 0 {
        return Ok(Err("в картинке нет точек".into()));
    }
    let (w, h) = scaled(width, height, max_side);
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(w)?;
    transform.SetScaledHeight(h)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    // The turn of a photo is applied below, by hand: the order of the
    // scale and the turn inside Windows is not documented.
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &transform,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::ColorManageToSRgb,
        )?
        .join()?;
    let pixels = data.DetachPixelData()?.to_vec();
    if pixels.len() != w as usize * h as usize * 4 {
        return Ok(Err("Windows отдал не столько точек, сколько обещал".into()));
    }
    let turn = orientation(&decoder).unwrap_or(1);
    let (pixels, w, h) = orient(&pixels, w, h, turn);
    Ok(encode(&pixels, w, h).map(|bytes| Png {
        bytes,
        width: w,
        height: h,
    }))
}

/// The EXIF turn of a photo, 1 to 8; `None` when the picture has none.
fn orientation(decoder: &BitmapDecoder) -> Option<u16> {
    let keys: windows_collections::IIterable<HSTRING> = vec![HSTRING::from(ORIENTATION)].into();
    let found = decoder
        .BitmapProperties()
        .ok()?
        .GetPropertiesAsync(&keys)
        .ok()?
        .join()
        .ok()?;
    let value = found
        .Lookup(&HSTRING::from(ORIENTATION))
        .ok()?
        .Value()
        .ok()?;
    value.cast::<IReference<u16>>().ok()?.Value().ok()
}

/// RGBA pixels turned upright by the EXIF mark `turn` (1 to 8; anything
/// else leaves them as they are). Returns the pixels and the new size.
pub fn orient(rgba: &[u8], w: u32, h: u32, turn: u16) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let swap = matches!(turn, 5..=8);
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    if !(2..=8).contains(&turn) {
        return (rgba.to_vec(), w as u32, h as u32);
    }
    let mut out = vec![0u8; rgba.len()];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match turn {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            let from = (sy * w + sx) * 4;
            let to = (y * ow + x) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    (out, ow as u32, oh as u32)
}

/// PNG of RGBA pixels; a picture without see-through points is kept as RGB.
fn encode(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let points = rgba.as_chunks::<4>().0;
    if points.iter().all(|p| p[3] == 255) {
        let rgb: Vec<u8> = points.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        return super::screen::encode_png(&rgb, w, h);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// A PNG of RGB points, for tests elsewhere.
#[cfg(test)]
pub(crate) fn tests_png(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    super::screen::encode_png(rgb, w, h).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size of a PNG and whether it is RGBA.
    fn png_info(bytes: &[u8]) -> (u32, u32, png::ColorType) {
        let reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let info = reader.info();
        (info.width, info.height, info.color_type)
    }

    /// A 24-bit BMP of `w`×`h` red points.
    fn bmp(w: u32, h: u32) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let size = 54 + row * h;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&(h as i32).to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        for _ in 0..h {
            let mut line: Vec<u8> = (0..w).flat_map(|_| [0u8, 0, 255]).collect();
            line.resize(row as usize, 0);
            b.extend_from_slice(&line);
        }
        b
    }

    #[test]
    fn a_big_picture_is_made_smaller_and_kept_as_png() {
        let rgb = vec![90u8; 3200 * 1000 * 3];
        let big = super::super::screen::encode_png(&rgb, 3200, 1000).unwrap();
        let out = to_png(big, 1600).unwrap();
        assert_eq!((out.width, out.height), (1600, 500));
        assert_eq!(png_info(&out.bytes), (1600, 500, png::ColorType::Rgb));
    }

    #[test]
    fn a_bmp_becomes_png_of_the_same_size() {
        let out = to_png(bmp(3, 2), 1600).unwrap();
        assert!(out.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        assert_eq!(png_info(&out.bytes), (3, 2, png::ColorType::Rgb));
    }

    #[test]
    fn something_that_is_no_picture_is_refused() {
        let err = to_png(b"just some text".to_vec(), 1600).unwrap_err();
        assert!(err.starts_with("картинка не прочитана"), "{err}");
    }

    #[test]
    fn sizes_fit_the_long_side() {
        assert_eq!(scaled(3200, 1000, 1600), (1600, 500));
        assert_eq!(scaled(1000, 4000, 1600), (400, 1600));
        assert_eq!(scaled(800, 600, 1600), (800, 600));
        assert_eq!(scaled(10_000, 1, 1600), (1600, 1));
    }

    #[test]
    fn photos_are_turned_upright() {
        // 3×2: points numbered row by row.
        let px: Vec<u8> = (0..6u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let ids = |v: &[u8]| v.chunks(4).map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(orient(&px, 3, 2, 1), (px.clone(), 3, 2));
        let (o, w, h) = orient(&px, 3, 2, 3);
        assert_eq!((ids(&o), w, h), (vec![5, 4, 3, 2, 1, 0], 3, 2));
        // Turned a quarter clockwise: the first column becomes the first row, from the bottom up.
        let (o, w, h) = orient(&px, 3, 2, 6);
        assert_eq!((ids(&o), w, h), (vec![3, 0, 4, 1, 5, 2], 2, 3));
        let (o, w, h) = orient(&px, 3, 2, 8);
        assert_eq!((ids(&o), w, h), (vec![2, 5, 1, 4, 0, 3], 2, 3));
        let (o, _, _) = orient(&px, 3, 2, 2);
        assert_eq!(ids(&o), vec![2, 1, 0, 5, 4, 3]);
        let (o, w, h) = orient(&px, 3, 2, 5);
        assert_eq!((ids(&o), w, h), (vec![0, 3, 1, 4, 2, 5], 2, 3));
    }
}
