use std::io::Cursor;
use std::path::Path;
use anyhow::{Context, Result};
use image::DynamicImage;

pub const THUMB_SIDE: u32 = 256;

pub struct Meta { pub w: u32, pub h: u32, pub taken_at: Option<String> }

pub fn meta(path: &Path) -> Result<Meta> {
    let (w, h) = image::ImageReader::open(path)?.with_guessed_format()?.into_dimensions()?;
    let bytes = std::fs::read(path)?;
    Ok(Meta { w, h, taken_at: exif(&bytes).and_then(|e| exif_date(&e)) })
}

fn exif(bytes: &[u8]) -> Option<exif::Exif> {
    exif::Reader::new().read_from_container(&mut Cursor::new(bytes)).ok()
}
fn exif_date(e: &exif::Exif) -> Option<String> {
    let f = e.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    let s = f.display_value().to_string();               // "2021-07-04 15:33:12"
    let dt = chrono::NaiveDateTime::parse_from_str(s.trim_matches('"'), "%Y-%m-%d %H:%M:%S").ok()?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
}
fn orientation(bytes: &[u8]) -> u32 {
    exif(bytes).and_then(|e| e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))).unwrap_or(1)
}
fn apply_orientation(img: DynamicImage, o: u32) -> DynamicImage {
    match o {
        2 => img.fliph(), 3 => img.rotate180(), 4 => img.flipv(),
        5 => img.fliph().rotate270(), 6 => img.rotate90(),
        7 => img.fliph().rotate90(),  8 => img.rotate270(), _ => img,
    }
}

/// SIMD-ресайз. Если версия fast_image_resize другая — правьте только эту функцию
/// (запасной вариант: image::imageops::resize(&img, w, h, image::imageops::FilterType::CatmullRom)).
fn resize(img: &DynamicImage, nw: u32, nh: u32) -> Result<DynamicImage> {
    use fast_image_resize as fr;
    let src = img.to_rgba8();
    let (w, h) = src.dimensions();
    let s = fr::images::Image::from_vec_u8(w, h, src.into_raw(), fr::PixelType::U8x4)?;
    let mut d = fr::images::Image::new(nw.max(1), nh.max(1), fr::PixelType::U8x4);
    fr::Resizer::new().resize(&s, &mut d,
        &fr::ResizeOptions::new().resize_alg(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3)))?;
    let out = image::RgbaImage::from_raw(nw.max(1), nh.max(1), d.into_vec()).context("resize buf")?;
    Ok(DynamicImage::ImageRgba8(out))
}

fn fit(img: DynamicImage, max_side: u32) -> Result<DynamicImage> {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max_side { return Ok(img); }
    let k = max_side as f32 / w.max(h) as f32;
    resize(&img, ((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1))
}

/// Декод с применением EXIF-ориентации и уменьшением до max_side.
pub fn decode_rgba(path: &Path, max_side: u32) -> Result<(u32, u32, Vec<u8>)> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let img = image::load_from_memory(&bytes).with_context(|| format!("decode {}", path.display()))?;
    let img = fit(apply_orientation(img, orientation(&bytes)), max_side)?;
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

fn save_jpeg(img: &DynamicImage, path: &Path, q: u8) -> Result<()> {
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    let rgb = img.to_rgb8();
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(f, q)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)?;
    Ok(())
}

/// Один полный декод → две производные (экран + превью из уже уменьшенной).
pub fn make_derivatives(src: &Path, thumb: &Path, screen: &Path, side: u32) -> Result<()> {
    let bytes = std::fs::read(src)?;
    let img = apply_orientation(image::load_from_memory(&bytes)?, orientation(&bytes));
    let scr = fit(img, side)?;
    save_jpeg(&scr, screen, 90)?;
    let th = fit(scr, THUMB_SIDE)?;
    save_jpeg(&th, thumb, 82)
}

pub fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg", "png" => "image/png", "webp" => "image/webp",
        "bmp" => "image/bmp", "tif" | "tiff" => "image/tiff", _ => "application/octet-stream",
    }
}
pub fn is_supported(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "bmp" | "tif" | "tiff")
    // TODO: heic/heif — через libheif-rs (системная зависимость, отдельная сборка)
}
