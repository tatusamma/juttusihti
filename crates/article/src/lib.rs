//! JSON contract between the app and the Servo-based browser helper, plus picture
//! shrinking and text normalization. Has no Servo dependency.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_PICTURE_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const PICTURE_MAX_WIDTH: u32 = 1600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    pub want_picture: bool,
    pub profile_dir: PathBuf,
    pub picture_path: PathBuf,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PictureInfo {
    pub path: PathBuf,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum FetchOutcome {
    Fetched { text: String, paywalled: bool, picture: Option<PictureInfo> },
    Refused { status: u16 },
    Timeout,
    NoArticle,
    Failed { message: String },
}

/// Decodes `input` (JPEG, PNG, WebP or GIF), shrinks it to at most `PICTURE_MAX_WIDTH`
/// pixels wide and writes it to `out` as a JPEG. Returns `None`, writing nothing, when the
/// input is too large, cannot be decoded or cannot be written.
pub fn process_picture(input: &[u8], out: &Path) -> Option<PictureInfo> {
    use image::imageops::FilterType;

    if input.len() > MAX_PICTURE_INPUT_BYTES {
        return None;
    }
    let decoded = image::load_from_memory(input).ok()?;
    let resized = if decoded.width() > PICTURE_MAX_WIDTH {
        let height =
            (u64::from(decoded.height()) * u64::from(PICTURE_MAX_WIDTH) / u64::from(decoded.width())).max(1) as u32;
        decoded.resize_exact(PICTURE_MAX_WIDTH, height, FilterType::Lanczos3)
    } else {
        decoded
    };
    let rgb = image::DynamicImage::ImageRgb8(resized.to_rgb8());

    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80);
    rgb.write_with_encoder(encoder).ok()?;
    std::fs::write(out, &buf).ok()?;
    Some(PictureInfo { path: out.to_path_buf(), bytes: buf.len() as u64, width: rgb.width(), height: rgb.height() })
}

/// Trims every line and collapses runs of blank lines into one, keeping paragraph breaks.
pub fn normalize_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_pending = false;
    for line in raw.lines().map(str::trim) {
        if line.is_empty() {
            blank_pending = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank_pending {
                "

"
            } else {
                "
"
            });
        }
        blank_pending = false;
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbImage};
    use std::io::Cursor;

    fn encode(img: &DynamicImage, fmt: ImageFormat) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, fmt).unwrap();
        buf.into_inner()
    }

    fn gradient(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        }))
    }

    #[test]
    fn request_and_outcome_round_trip_json() {
        let req = FetchRequest {
            url: "https://example.com/a".into(),
            want_picture: true,
            profile_dir: "profile".into(),
            picture_path: "pic.jpg".into(),
            timeout_secs: 30,
        };
        let back: FetchRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(req, back);

        let pic = PictureInfo { path: "pic.jpg".into(), bytes: 10, width: 4, height: 3 };
        let outcomes = vec![
            FetchOutcome::Fetched { text: "t".into(), paywalled: true, picture: Some(pic) },
            FetchOutcome::Fetched { text: "t".into(), paywalled: false, picture: None },
            FetchOutcome::Refused { status: 403 },
            FetchOutcome::Timeout,
            FetchOutcome::NoArticle,
            FetchOutcome::Failed { message: "boom".into() },
        ];
        for o in outcomes {
            let json = serde_json::to_string(&o).unwrap();
            assert!(json.contains("\"kind\""), "{json}");
            let back: FetchOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(o, back);
        }
    }

    #[test]
    fn jpeg_and_webp_are_resized_to_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let img = gradient(3000, 1500);
        let jpeg = encode(&img, ImageFormat::Jpeg);
        let webp = encode(&img, ImageFormat::WebP);
        for (name, data) in [("a.jpg", jpeg), ("b.jpg", webp)] {
            let out = dir.path().join(name);
            let info = process_picture(&data, &out).expect("picture");
            assert_eq!(info.path, out);
            assert!(info.width <= PICTURE_MAX_WIDTH);
            assert_eq!(info.width, 1600);
            assert_eq!(info.height, 800);
            assert_eq!(info.bytes, std::fs::metadata(&out).unwrap().len());
            let written = std::fs::read(&out).unwrap();
            assert_eq!(image::guess_format(&written).unwrap(), ImageFormat::Jpeg);
        }
    }

    #[test]
    fn small_picture_is_not_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("s.jpg");
        let png = encode(&gradient(200, 100), ImageFormat::Png);
        let info = process_picture(&png, &out).unwrap();
        assert_eq!((info.width, info.height), (200, 100));
    }

    #[test]
    fn undecodable_or_oversized_picture_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut avif = vec![0, 0, 0, 0x1c];
        avif.extend_from_slice(b"ftypavif\0\0\0\0avifmif1miaf");
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".to_vec();
        let big = vec![0u8; 9 * 1024 * 1024];
        for (i, data) in [avif, svg, big].iter().enumerate() {
            let out = dir.path().join(format!("{i}.jpg"));
            assert!(process_picture(data, &out).is_none());
            assert!(!out.exists());
        }
    }

    #[test]
    fn normalize_text_keeps_paragraphs() {
        assert_eq!(normalize_text("  a\n\n\n b \n\nc  "), "a\n\nb\n\nc");
    }
}
