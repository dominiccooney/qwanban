use std::fs::File;
use image::RgbaImage;
use rav1e::prelude::*;
use tokio::time::Instant;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};
use webm_iterable::WebmWriter;
use crate::pal;

/// Number of nanoseconds represented by one tick of the timestamps written
/// into this file (Cluster/Timestamp and Block timestamps). 1,000,000ns = 1ms
/// per tick, which is the conventional value used by most webm muxers.
const TIMESTAMP_SCALE_NANOS: u64 = 1_000_000;

/// Converts an interleaved RGBA screenshot into planar YUV 4:2:0 buffers
/// suitable for feeding into a rav1e `Frame`.
///
/// rav1e (like all AV1 encoders) works on planar Y/U/V data, not interleaved
/// RGBA, and the U/V (chroma) planes are subsampled to half resolution in
/// both dimensions for 4:2:0. This uses the standard limited-range BT.601
/// coefficients (the same ones implied by rav1e's default `PixelRange::Limited`).
///
/// Returns `(y_plane, u_plane, v_plane, chroma_width, chroma_height)`, where
/// `y_plane` is `width * height` bytes and `u_plane`/`v_plane` are each
/// `chroma_width * chroma_height` bytes, all tightly packed (no padding).
fn rgba_to_yuv420(image: &RgbaImage) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize, usize) {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let chroma_width = (width + 1) / 2;
    let chroma_height = (height + 1) / 2;

    let mut y_plane = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let p = image.get_pixel(x as u32, y as u32);
            let (r, g, b) = (p.0[0] as f32, p.0[1] as f32, p.0[2] as f32);
            let y_val = 0.257 * r + 0.504 * g + 0.098 * b + 16.0;
            y_plane[y * width + x] = y_val.round().clamp(0.0, 255.0) as u8;
        }
    }

    let mut u_plane = vec![0u8; chroma_width * chroma_height];
    let mut v_plane = vec![0u8; chroma_width * chroma_height];
    for cy in 0..chroma_height {
        for cx in 0..chroma_width {
            // Average the 2x2 block of source pixels this chroma sample covers,
            // clamping at the image edges for odd width/height images.
            let x0 = (cx * 2).min(width - 1);
            let x1 = (cx * 2 + 1).min(width - 1);
            let y0 = (cy * 2).min(height - 1);
            let y1 = (cy * 2 + 1).min(height - 1);

            let mut r_sum = 0.0f32;
            let mut g_sum = 0.0f32;
            let mut b_sum = 0.0f32;
            for &(xx, yy) in &[(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let p = image.get_pixel(xx as u32, yy as u32);
                r_sum += p.0[0] as f32;
                g_sum += p.0[1] as f32;
                b_sum += p.0[2] as f32;
            }
            let (r, g, b) = (r_sum / 4.0, g_sum / 4.0, b_sum / 4.0);

            let u_val = -0.148 * r - 0.291 * g + 0.439 * b + 128.0;
            let v_val = 0.439 * r - 0.368 * g - 0.071 * b + 128.0;
            u_plane[cy * chroma_width + cx] = u_val.round().clamp(0.0, 255.0) as u8;
            v_plane[cy * chroma_width + cx] = v_val.round().clamp(0.0, 255.0) as u8;
        }
    }

    (y_plane, u_plane, v_plane, chroma_width, chroma_height)
}

pub(crate) async fn encode_video_demo() -> anyhow::Result<()> {
    let start_time = Instant::now();
    let image = pal::screenshot()?;
    let mut writer = WebmWriter::new(File::create("video.webm")?);

    writer.write(&MatroskaSpec::Ebml(Master::Start))?;
    writer.write(&MatroskaSpec::DocType(String::from("webm")))?;
    writer.write(&MatroskaSpec::Ebml(Master::End))?;

    writer.write(&MatroskaSpec::Segment(Master::Start))?;

    writer.write(&MatroskaSpec::Info(Master::Start))?;
    writer.write(&MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NANOS))?;
    writer.write(&MatroskaSpec::Info(Master::End))?;

    // Set up a rav1e context
    let encoder_config = rav1e::Config::new().with_encoder_config(rav1e::EncoderConfig {
        width: image.width() as usize,
        height: image.height() as usize,
        speed_settings: SpeedSettings::from_preset(10),
        ..Default::default()
    });
    let mut encoder_context: rav1e::Context<u16> = encoder_config.new_context()?;

    // The AV1 CodecPrivate blob required by the Matroska/WebM AV1 mapping is
    // exactly the "AV1 sequence header" that rav1e can produce for us from
    // the encoder config, so we don't have to build it by hand.
    let codec_private = encoder_context.container_sequence_header();

    writer.write(&MatroskaSpec::Tracks(Master::Start))?;

    // Video track
    let video_track_number: u64 = 1;
    writer.write(&MatroskaSpec::TrackEntry(Master::Start))?;
    writer.write(&MatroskaSpec::TrackNumber(video_track_number))?;
    writer.write(&MatroskaSpec::TrackUID(video_track_number))?;
    writer.write(&MatroskaSpec::TrackType(1))?; // 1 = video
    writer.write(&MatroskaSpec::CodecID(String::from("V_AV1")))?;
    writer.write(&MatroskaSpec::CodecPrivate(codec_private))?;
    writer.write(&MatroskaSpec::Video(Master::Start))?;
    writer.write(&MatroskaSpec::PixelWidth(image.width() as u64))?;
    writer.write(&MatroskaSpec::PixelHeight(image.height() as u64))?;
    writer.write(&MatroskaSpec::Video(Master::End))?;
    writer.write(&MatroskaSpec::TrackEntry(Master::End))?;

    // TODO: Consider adding an audio track (type 2), e.g. A_OPUS codec

    writer.write(&MatroskaSpec::Tracks(Master::End))?;

    // Encode a frame. AV1/rav1e needs planar YUV 4:2:0 data, so convert the
    // interleaved RGBA screenshot first, then copy each plane in separately
    // using that plane's own (tightly-packed) width as the source stride.
    let (y_plane, u_plane, v_plane, chroma_width, _chroma_height) = rgba_to_yuv420(&image);
    let mut frame = encoder_context.new_frame();
    frame.planes[0].copy_from_raw_u8(&y_plane, image.width() as usize, 1);
    frame.planes[1].copy_from_raw_u8(&u_plane, chroma_width, 1);
    frame.planes[2].copy_from_raw_u8(&v_plane, chroma_width, 1);
    encoder_context.send_frame(frame)?;

    encoder_context.flush();

    // This demo only ever produces a single Cluster, starting at timestamp 0.
    writer.write(&MatroskaSpec::Cluster(Master::Start))?;
    writer.write(&MatroskaSpec::Timestamp(0))?;

    // Pull packets out and mux each one into the Cluster as a SimpleBlock.
    'encoding: loop {
        match encoder_context.receive_packet() {
            Ok(packet) => {
                // Block timestamps are relative to the enclosing Cluster's
                // Timestamp, in TimestampScale units, and stored as an i16.
                let block_timestamp: i16 = 0;
                let simple_block = SimpleBlock::new_uncheked(
                    &packet.data,
                    video_track_number,
                    block_timestamp,
                    false,
                    None,
                    false,
                    packet.frame_type == FrameType::KEY,
                );
                writer.write(&MatroskaSpec::from(simple_block))?;
            }
            Err(err) => match err {
                EncoderStatus::LimitReached => {
                    println!("limit reached!");
                    break 'encoding;
                }
                EncoderStatus::Encoded => {
                    println!("encoded");
                }
                EncoderStatus::NeedMoreData => {
                    println!("need more data");
                    break 'encoding;
                }
                _ => {
                    anyhow::bail!(err);
                }
            }
        }
    }

    writer.write(&MatroskaSpec::Cluster(Master::End))?;
    writer.write(&MatroskaSpec::Segment(Master::End))?;
    writer.flush()?;

    println!("Encoded video in {:?}", start_time.elapsed());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use webm_iterable::WebmIterator;

    #[tokio::test]
    async fn encode_video_demo_produces_valid_webm() {
        encode_video_demo().await.expect("encoding should succeed");

        let bytes = std::fs::read("video.webm").expect("video.webm should have been written");
        assert!(!bytes.is_empty());

        let mut src = Cursor::new(bytes);
        let iterator = WebmIterator::new(&mut src, &[]);
        let tags: Vec<MatroskaSpec> = iterator
            .collect::<Result<Vec<_>, _>>()
            .expect("file should be a well-formed EBML/Matroska stream");

        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::Segment(_))));
        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::TrackEntry(_))));
        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::CodecID(id) if id == "V_AV1")));
        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::CodecPrivate(data) if !data.is_empty())));
        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::Cluster(_))));
        assert!(tags.iter().any(|t| matches!(t, MatroskaSpec::SimpleBlock(_))));
    }

    /// Helper: byte-approximate equality, since the conversion involves
    /// floating point rounding.
    fn approx(actual: u8, expected: u8) {
        let diff = (actual as i16 - expected as i16).abs();
        assert!(
            diff <= 1,
            "expected ~{expected}, got {actual} (diff {diff})"
        );
    }

    #[test]
    fn rgba_to_yuv420_produces_correctly_sized_planes() {
        // 4x2 image => chroma planes should be 2x1 (half width, half height).
        let image = RgbaImage::from_pixel(4, 2, image::Rgba([128, 128, 128, 255]));
        let (y, u, v, chroma_width, chroma_height) = rgba_to_yuv420(&image);

        assert_eq!(y.len(), 4 * 2);
        assert_eq!(chroma_width, 2);
        assert_eq!(chroma_height, 1);
        assert_eq!(u.len(), chroma_width * chroma_height);
        assert_eq!(v.len(), chroma_width * chroma_height);
    }

    #[test]
    fn rgba_to_yuv420_converts_known_colors() {
        // A solid-color image should produce a uniform Y plane matching the
        // BT.601 limited-range luma value, and U/V matching mid-gray (128)
        // for black/white/gray, since those are colorless (no chroma).
        let black = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255]));
        let (y, u, v, _, _) = rgba_to_yuv420(&black);
        for &val in &y {
            approx(val, 16);
        }
        for &val in u.iter().chain(v.iter()) {
            approx(val, 128);
        }

        let white = RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255]));
        let (y, u, v, _, _) = rgba_to_yuv420(&white);
        for &val in &y {
            approx(val, 235);
        }
        for &val in u.iter().chain(v.iter()) {
            approx(val, 128);
        }

        // Pure red (BT.601 limited range): Y ~= 81, U ~= 90, V ~= 240.
        let red = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let (y, u, v, _, _) = rgba_to_yuv420(&red);
        for &val in &y {
            approx(val, 81);
        }
        for &val in &u {
            approx(val, 90);
        }
        for &val in &v {
            approx(val, 240);
        }
    }
}
