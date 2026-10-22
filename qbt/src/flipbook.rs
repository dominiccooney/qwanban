use std::path::Path;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use webp_animation::{Encoder, EncoderOptions, EncodingConfig};

const WEBP_QUALITY: f32 = 75.0;
const RANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// The final frame stays up this long before the animation loops, so a
/// reader has time to take in the end state.
pub(crate) const LAST_FRAME_HOLD_MS: i32 = 5000;
/// Captioned frames are shrunk to this fraction of the screen so the file
/// stays small enough to drop into a pull request comment.
const CAPTIONED_FRAME_SCALE: f32 = 0.5;
const CAPTION_FONT: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");

pub(crate) async fn create(
    observatory: &str,
    from: &str,
    to: &str,
    out: &Path,
    fps: f64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        fps.is_finite() && fps > 0.0 && fps <= 1000.0,
        "fps must be greater than zero and at most 1000"
    );
    let (mut websocket, _) = tokio_tungstenite::connect_async(observatory)
        .await
        .with_context(|| format!("connecting to observatory at {observatory}"))?;
    websocket
        .send(Message::Text(
            serde_json::json!({ "fetchScreenshotRange": { "from": from, "to": to } })
                .to_string()
                .into(),
        ))
        .await?;

    let pngs = tokio::time::timeout(RANGE_TIMEOUT, async {
        let mut pngs = Vec::new();
        let mut ids = Vec::new();
        loop {
            let message = websocket.next().await.ok_or_else(|| {
                anyhow::anyhow!("observatory disconnected before completing the range")
            })??;
            match message {
                Message::Binary(bytes) => {
                    let newline =
                        bytes
                            .iter()
                            .position(|byte| *byte == b'\n')
                            .ok_or_else(|| {
                                anyhow::anyhow!("invalid screenshot frame from observatory")
                            })?;
                    let id = std::str::from_utf8(&bytes[..newline])?.to_owned();
                    let sequence = screenshot_sequence(&id)?;
                    anyhow::ensure!(
                        ids.last().is_none_or(|(_, previous)| sequence > *previous),
                        "observatory returned screenshots outside journal order"
                    );
                    ids.push((id, sequence));
                    pngs.push(bytes[newline + 1..].to_vec());
                }
                Message::Text(text) => {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    if let Some(error) = value
                        .get("screenshotRangeError")
                        .and_then(|value| value.as_str())
                    {
                        anyhow::bail!(error.to_owned());
                    }
                    if let Some(expected_count) = value
                        .get("screenshotRangeComplete")
                        .and_then(|value| value.as_u64())
                    {
                        anyhow::ensure!(
                            pngs.len() as u64 == expected_count,
                            "observatory returned {} of {expected_count} screenshots",
                            pngs.len()
                        );
                        anyhow::ensure!(
                            ids.first().map(|(id, _)| id.as_str()) == Some(from)
                                && ids.last().map(|(id, _)| id.as_str()) == Some(to),
                            "observatory returned the wrong screenshot range"
                        );
                        break Ok(pngs);
                    }
                }
                Message::Close(_) => {
                    anyhow::bail!("observatory disconnected before completing the range")
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for screenshot range"))??;
    let data = encode(&pngs, fps, None)?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &data)?;
    Ok(())
}

fn screenshot_sequence(id: &str) -> anyhow::Result<u64> {
    id.strip_prefix("shot_")
        .ok_or_else(|| anyhow::anyhow!("invalid screenshot id from observatory: {id}"))?
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid screenshot id from observatory: {id}"))
}

fn frame_timestamp(frame_index: usize, fps: f64) -> anyhow::Result<i32> {
    let timestamp = ((frame_index as f64 * 1000.0) / fps).round();
    anyhow::ensure!(
        timestamp <= i32::MAX as f64,
        "flipbook duration is too long"
    );
    Ok(timestamp as i32)
}

/// Encodes PNG frames into one animated WebP and returns its bytes. With
/// `captions` (one per frame) each frame is shrunk to
/// `CAPTIONED_FRAME_SCALE` and its caption is drawn along the bottom. Frames
/// are decoded one at a time, so the count is bounded by disk, not memory.
pub(crate) fn encode(
    pngs: &[Vec<u8>],
    fps: f64,
    captions: Option<&[String]>,
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(!pngs.is_empty(), "screenshot range is empty");
    if let Some(captions) = captions {
        anyhow::ensure!(
            captions.len() == pngs.len(),
            "expected one caption per frame"
        );
    }
    let scale = if captions.is_some() {
        CAPTIONED_FRAME_SCALE
    } else {
        1.0
    };
    let dimensions = pngs
        .iter()
        .map(|png| {
            image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
                .into_dimensions()
                .map(|(width, height)| scaled_dimensions(width, height, scale))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let canvas_dimensions =
        dimensions
            .iter()
            .fold((0, 0), |(width, height), (frame_width, frame_height)| {
                (width.max(*frame_width), height.max(*frame_height))
            });
    let options = EncoderOptions {
        encoding_config: Some(EncodingConfig::new_lossy(WEBP_QUALITY)),
        ..Default::default()
    };
    let mut encoder = Encoder::new_with_options(canvas_dimensions, options)?;
    for (index, png) in pngs.iter().enumerate() {
        let mut frame =
            image::load_from_memory_with_format(png, image::ImageFormat::Png)?.to_rgba8();
        if scale != 1.0 {
            let (width, height) = dimensions[index];
            frame = image::imageops::resize(
                &frame,
                width,
                height,
                image::imageops::FilterType::Triangle,
            );
        }
        if frame.dimensions() != canvas_dimensions {
            let mut canvas = image::RgbaImage::from_pixel(
                canvas_dimensions.0,
                canvas_dimensions.1,
                image::Rgba([0, 0, 0, 255]),
            );
            image::imageops::overlay(&mut canvas, &frame, 0, 0);
            frame = canvas;
        }
        if let Some(captions) = captions {
            caption(&mut frame, index + 1, pngs.len(), &captions[index]);
        }
        encoder.add_frame(frame.as_raw(), frame_timestamp(index, fps)?)?;
    }
    let end = frame_timestamp(pngs.len() - 1, fps)?
        .checked_add(LAST_FRAME_HOLD_MS)
        .ok_or_else(|| anyhow::anyhow!("flipbook duration is too long"))?;
    Ok(encoder.finalize(end)?.to_vec())
}

fn scaled_dimensions(width: u32, height: u32, scale: f32) -> (u32, u32) {
    (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    )
}

/// Draws "N/T label" in a translucent band along the bottom of the frame.
/// Labels wider than the band are cut with an ellipsis.
fn caption(frame: &mut image::RgbaImage, index: usize, total: usize, label: &str) {
    use ab_glyph::{Font, FontRef, ScaleFont};

    let (width, height) = frame.dimensions();
    let band_height = ((height as f32) * 0.06).round().clamp(14.0, 96.0) as u32;
    let band_height = band_height.min(height);
    let band_top = height - band_height;
    for y in band_top..height {
        for x in 0..width {
            let pixel = frame.get_pixel_mut(x, y);
            for channel in &mut pixel.0[..3] {
                *channel = (*channel as f32 * 0.3) as u8;
            }
        }
    }

    let font = FontRef::try_from_slice(CAPTION_FONT).expect("bundled caption font is valid");
    let scaled = font.as_scaled(band_height as f32 * 0.6);
    let padding = band_height as f32 * 0.3;
    let baseline = band_top as f32 + padding + scaled.ascent();
    let text = format!("{index}/{total}  {label}");
    let advance = |c: char| scaled.h_advance(font.glyph_id(c));
    let ellipsis_width = advance('…');
    let max_width = width as f32 - 2.0 * padding;
    let mut fitted = String::new();
    let mut text_width = 0.0;
    let mut truncated = false;
    for c in text.chars() {
        let next = text_width + advance(c);
        if next + ellipsis_width > max_width {
            truncated = true;
            break;
        }
        fitted.push(c);
        text_width = next;
    }
    if truncated {
        fitted.push('…');
    }

    let mut x = padding;
    for c in fitted.chars() {
        let glyph_id = font.glyph_id(c);
        let glyph = glyph_id.with_scale_and_position(scaled.scale(), ab_glyph::point(x, baseline));
        if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            outline.draw(|px, py, coverage| {
                let x = bounds.min.x as i64 + px as i64;
                let y = bounds.min.y as i64 + py as i64;
                if x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
                    return;
                }
                let pixel = frame.get_pixel_mut(x as u32, y as u32);
                for channel in &mut pixel.0[..3] {
                    *channel = (*channel as f32 + (255.0 - *channel as f32) * coverage) as u8;
                }
            });
        }
        x += scaled.h_advance(glyph_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{DEFAULT_MAX_SCREENSHOTS, Journal};
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;
    use tokio_util::sync::CancellationToken;

    fn png(color: Rgba<u8>) -> Vec<u8> {
        png_with_dimensions(2, 2, color)
    }

    fn png_with_dimensions(width: u32, height: u32, color: Rgba<u8>) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, color);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn encodes_an_animated_lossy_webp() {
        let bytes = encode(
            &[png(Rgba([255, 0, 0, 255])), png(Rgba([0, 0, 255, 255]))],
            2.0,
            None,
        )
        .unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(bytes.windows(4).any(|window| window == b"ANIM"));
        assert!(bytes.windows(4).any(|window| window == b"ANMF"));
    }

    #[test]
    fn cumulative_timestamps_do_not_accumulate_rounding_error() {
        assert_eq!(frame_timestamp(600, 600.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(24, 24.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(3, 3.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(1, 600.0).unwrap(), 2);
        assert_eq!(frame_timestamp(2, 600.0).unwrap(), 3);
    }

    fn frame_timestamps(bytes: &[u8]) -> Vec<i32> {
        webp_animation::Decoder::new(bytes)
            .unwrap()
            .into_iter()
            .map(|frame| frame.timestamp())
            .collect()
    }

    #[test]
    fn holds_the_last_frame_before_looping() {
        let frames = [
            png(Rgba([255, 0, 0, 255])),
            png(Rgba([0, 255, 0, 255])),
            png(Rgba([0, 0, 255, 255])),
        ];
        let bytes = encode(&frames, 1.0, None).unwrap();
        // Decoded timestamps are each frame's end time.
        assert_eq!(
            frame_timestamps(&bytes),
            vec![1000, 2000, 2000 + LAST_FRAME_HOLD_MS]
        );
    }

    #[test]
    fn captioned_frames_are_downscaled_and_carry_a_band() {
        let frames = [
            png_with_dimensions(200, 100, Rgba([255, 255, 255, 255])),
            png_with_dimensions(200, 100, Rgba([255, 255, 255, 255])),
        ];
        let captions = ["Open the editor".to_owned(), "x".repeat(500)];
        let bytes = encode(&frames, 1.0, Some(&captions)).unwrap();
        let decoder = webp_animation::Decoder::new(&bytes).unwrap();
        assert_eq!(decoder.dimensions(), (100, 50));
        let frame = decoder.into_iter().next().unwrap();
        let (width, height) = frame.dimensions();
        let pixel = |x: u32, y: u32| &frame.data()[((y * width + x) * 4) as usize..][..3];
        assert_eq!(pixel(width / 2, 2), [255, 255, 255], "top stays white");
        assert!(
            pixel(width - 2, height - 2)[0] < 128,
            "caption band darkens the bottom"
        );
    }

    #[test]
    fn caption_count_must_match_frames() {
        let error = encode(&[png(Rgba([0, 0, 0, 255]))], 1.0, Some(&[]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("one caption per frame"));
    }

    #[test]
    fn pads_changing_desktop_sizes_one_frame_at_a_time() {
        let bytes = encode(
            &[
                png_with_dimensions(2, 1, Rgba([255, 0, 0, 255])),
                png_with_dimensions(1, 2, Rgba([0, 0, 255, 255])),
            ],
            2.0,
            None,
        )
        .unwrap();
        assert_eq!(
            webp_animation::Decoder::new(&bytes).unwrap().dimensions(),
            (2, 2)
        );
    }

    #[tokio::test]
    async fn fetches_the_requested_range_from_the_observatory() {
        let journal = Journal::new(DEFAULT_MAX_SCREENSHOTS);
        let first = journal.append(
            "test.first",
            serde_json::json!({}),
            Some(png(Rgba([255, 0, 0, 255]))),
        );
        let last = journal.append(
            "test.last",
            serde_json::json!({}),
            Some(png(Rgba([0, 0, 255, 255]))),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        tokio::spawn(crate::observed::serve_observatory(
            listener,
            journal,
            shutdown.clone(),
        ));
        let out =
            std::env::temp_dir().join(format!("qbt-flipbook-network-{}.webp", std::process::id()));

        create(
            &format!("ws://{address}"),
            first.screenshot_id.as_deref().unwrap(),
            last.screenshot_id.as_deref().unwrap(),
            &out,
            2.0,
        )
        .await
        .unwrap();

        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.windows(4).any(|window| window == b"ANIM"));
        shutdown.cancel();
        std::fs::remove_file(out).unwrap();
    }
}
