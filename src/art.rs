//! Album-art actor: download (with a disk cache), decode, crop square.

use std::path::PathBuf;

use image::DynamicImage;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug)]
pub struct ArtReq {
    pub key: String,
    pub url: String,
}

pub struct ArtEvent {
    pub key: String,
    pub image: Option<DynamicImage>,
}

pub(crate) fn cache_name(url: &str) -> String {
    // FNV-1a: stable, dependency-free file name for a URL.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}.img")
}

pub async fn run(cache_dir: PathBuf, mut reqs: UnboundedReceiver<ArtReq>, events: UnboundedSender<ArtEvent>) {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("http client");
    while let Some(req) = reqs.recv().await {
        let http = http.clone();
        let events = events.clone();
        let path = cache_dir.join(cache_name(&req.url));
        tokio::spawn(async move {
            let image = match load(&http, &req.url, path).await {
                Ok(img) => Some(img),
                Err(e) => {
                    tracing::info!("art {}: {e:#}", req.url);
                    None
                }
            };
            let _ = events.send(ArtEvent { key: req.key, image });
        });
    }
}

async fn decode_square(bytes: Vec<u8>) -> anyhow::Result<DynamicImage> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<DynamicImage> {
        let img = image::load_from_memory(&bytes)?;
        Ok(square(img))
    })
    .await?
}

async fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    let tmp = path.with_extension("img.tmp");
    if tokio::fs::write(&tmp, bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, path).await;
    }
}

pub(crate) async fn load(http: &reqwest::Client, url: &str, path: PathBuf) -> anyhow::Result<DynamicImage> {
    if let Ok(bytes) = tokio::fs::read(&path).await {
        match decode_square(bytes).await {
            Ok(img) => return Ok(img),
            Err(e) => {
                // Corrupt or truncated cache file: drop it and re-download.
                tracing::debug!("dropping corrupt art cache {}: {e}", path.display());
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
    let bytes = http.get(url).send().await?.error_for_status()?.bytes().await?.to_vec();
    let img = decode_square(bytes.clone()).await?;
    // Only cache bytes that actually decoded, atomically.
    write_atomic(&path, &bytes).await;
    Ok(img)
}

/// Centre-crop to a square (video thumbnails are 4:3/16:9) and cap the size.
fn square(img: DynamicImage) -> DynamicImage {
    let img = trim_letterbox(img);
    let (w, h) = (img.width(), img.height());
    let side = w.min(h);
    let img = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    if side > 720 {
        img.resize(720, 720, image::imageops::FilterType::Triangle)
    } else {
        img
    }
}

/// Remove black bars above/below the picture (YouTube `hqdefault` thumbnails).
fn trim_letterbox(img: DynamicImage) -> DynamicImage {
    let luma = img.to_luma8();
    let (w, h) = luma.dimensions();
    if w == 0 || h < 8 {
        return img;
    }
    let dark = |y: u32| -> bool {
        let sum: u64 = (0..w).map(|x| luma.get_pixel(x, y).0[0] as u64).sum();
        sum / (w as u64) < 14
    };
    let top = (0..h / 3).take_while(|&y| dark(y)).count() as u32;
    let bottom = (0..h / 3).take_while(|&i| dark(h - 1 - i)).count() as u32;
    if top + bottom == 0 || top + bottom >= h / 2 {
        return img;
    }
    img.crop_imm(0, top, w, h - top - bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn letterboxed_thumbnail_becomes_square_picture() {
        let mut img = RgbImage::new(480, 360);
        for y in 45..315 {
            for x in 0..480 {
                img.put_pixel(x, y, Rgb([200, 100, 50]));
            }
        }
        let out = square(DynamicImage::ImageRgb8(img));
        assert_eq!((out.width(), out.height()), (270, 270));
        let px = out.to_rgb8();
        assert_eq!(px.get_pixel(0, 0).0, [200, 100, 50]);
    }

    #[tokio::test]
    async fn corrupt_cache_file_is_removed_instead_of_staying_poisoned() {
        let dir = crate::app::test_paths().cache_dir;
        let path = dir.join("corrupt.img");
        tokio::fs::write(&path, b"not an image").await.unwrap();
        let http = reqwest::Client::new();
        // Download fails (invalid URL scheme), but the corrupt cache file must be removed.
        let res = load(&http, "http://127.0.0.1:1/unreachable.jpg", path.clone()).await;
        assert!(res.is_err());
        assert!(!path.exists(), "corrupt cache file should have been removed");
    }
}
