use std::path::Path;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use webp_animation::{Encoder, EncoderOptions, EncodingConfig};

const WEBP_QUALITY: f32 = 75.0;
const RANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    encode(&pngs, out, fps)
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

fn encode(pngs: &[Vec<u8>], out: &Path, fps: f64) -> anyhow::Result<()> {
    anyhow::ensure!(!pngs.is_empty(), "screenshot range is empty");
    let dimensions = pngs
        .iter()
        .map(|png| {
            image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
                .into_dimensions()
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
        let frame = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.to_rgba8();
        if frame.dimensions() == canvas_dimensions {
            encoder.add_frame(frame.as_raw(), frame_timestamp(index, fps)?)?;
        } else {
            let mut canvas = image::RgbaImage::from_pixel(
                canvas_dimensions.0,
                canvas_dimensions.1,
                image::Rgba([0, 0, 0, 255]),
            );
            image::imageops::overlay(&mut canvas, &frame, 0, 0);
            encoder.add_frame(canvas.as_raw(), frame_timestamp(index, fps)?)?;
        }
    }
    let data = encoder.finalize(frame_timestamp(pngs.len(), fps)?)?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &*data)?;
    Ok(())
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
        let out = std::env::temp_dir().join(format!("qbt-flipbook-{}.webp", std::process::id()));
        encode(
            &[png(Rgba([255, 0, 0, 255])), png(Rgba([0, 0, 255, 255]))],
            &out,
            2.0,
        )
        .unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(bytes.windows(4).any(|window| window == b"ANIM"));
        assert!(bytes.windows(4).any(|window| window == b"ANMF"));
        std::fs::remove_file(out).unwrap();
    }

    #[test]
    fn cumulative_timestamps_do_not_accumulate_rounding_error() {
        assert_eq!(frame_timestamp(600, 600.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(24, 24.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(3, 3.0).unwrap(), 1000);
        assert_eq!(frame_timestamp(1, 600.0).unwrap(), 2);
        assert_eq!(frame_timestamp(2, 600.0).unwrap(), 3);
    }

    #[test]
    fn pads_changing_desktop_sizes_one_frame_at_a_time() {
        let out = std::env::temp_dir().join(format!(
            "qbt-flipbook-changing-size-{}.webp",
            std::process::id()
        ));
        encode(
            &[
                png_with_dimensions(2, 1, Rgba([255, 0, 0, 255])),
                png_with_dimensions(1, 2, Rgba([0, 0, 255, 255])),
            ],
            &out,
            2.0,
        )
        .unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(
            webp_animation::Decoder::new(&bytes).unwrap().dimensions(),
            (2, 2)
        );
        std::fs::remove_file(out).unwrap();
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
