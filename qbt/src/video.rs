use std::fs::File;
use std::time::Duration;
use rav1e::prelude::*;
use tokio::time::Instant;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};
use webm_iterable::WebmWriter;
use yuvutils_rs::{YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix};
use crate::pal;

/// Number of nanoseconds represented by one tick of the timestamps written
/// into this file (Cluster/Timestamp and Block timestamps). 1,000,000ns = 1ms
/// per tick, which is the conventional value used by most webm muxers.
const TIMESTAMP_SCALE_NANOS: u64 = 1_000_000;
// Note: Because frames have 16-bit signed timestamp offsets, frame gaps longer than ~32 seconds
// need to start a new cluster.

pub(crate) async fn encode_video_demo() -> anyhow::Result<()> {
    let sampler = pal::ScreenSampler::new()?;

    let mut writer = WebmWriter::new(File::create("video.webm")?);

    writer.write(&MatroskaSpec::Ebml(Master::Start))?;
    writer.write(&MatroskaSpec::DocType(String::from("webm")))?;
    writer.write(&MatroskaSpec::Ebml(Master::End))?;

    writer.write(&MatroskaSpec::Segment(Master::Start))?;

    writer.write(&MatroskaSpec::Info(Master::Start))?;
    writer.write(&MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NANOS))?;
    writer.write(&MatroskaSpec::Info(Master::End))?;

    // Set up a rav1e context
    let (width, height) = sampler.size_px();
    let encoder_config = rav1e::Config::new().with_encoder_config(rav1e::EncoderConfig {
        width,
        height,
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
    writer.write(&MatroskaSpec::PixelWidth(width as u64))?;
    writer.write(&MatroskaSpec::PixelHeight(height as u64))?;
    writer.write(&MatroskaSpec::Video(Master::End))?;
    writer.write(&MatroskaSpec::TrackEntry(Master::End))?;

    // TODO: Consider adding an audio track (type 2), e.g. A_OPUS codec

    writer.write(&MatroskaSpec::Tracks(Master::End))?;

    let mut pixels_gbra8 = vec![0u8; sampler.pixel_buffer_size_u8()];
    let mut image_yuv = YuvPlanarImageMut::alloc(width as u32, height as u32, YuvChromaSubsampling::Yuv420);

    let goal_delay = Duration::from_secs(1) / 15;
    let start_time = Instant::now();
    let mut frame_times: Vec<Duration> = Vec::<Duration>::new();
    for i in 0..60 {
        // Encode a frame. AV1/rav1e needs planar YUV 4:2:0 data, so convert the
        // interleaved GRBA screen sample first.
        let this_frame_time = Instant::now();
        eprintln!("frame {}: {:?}", i, this_frame_time);
        sampler.sample(&mut pixels_gbra8)?;
        let stride_bgra = (width * 4) as u32;
        yuvutils_rs::bgra_to_yuv420(&mut image_yuv, &pixels_gbra8, stride_bgra, YuvRange::Limited, YuvStandardMatrix::Bt709, YuvConversionMode::Fast)?;

        let mut frame = encoder_context.new_frame();
        frame.planes[0].copy_from_raw_u8(image_yuv.y_plane.borrow(), image_yuv.y_stride as usize, 1);
        frame.planes[1].copy_from_raw_u8(image_yuv.u_plane.borrow(), image_yuv.u_stride as usize, 1);
        frame.planes[2].copy_from_raw_u8(image_yuv.v_plane.borrow(), image_yuv.v_stride as usize, 1);
        encoder_context.send_frame(frame)?;

        frame_times.push(this_frame_time - start_time);

        let submit_duration = Instant::now().duration_since(this_frame_time);
        if submit_duration < goal_delay {
            tokio::time::sleep(goal_delay - submit_duration).await;
        }
    }

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
                // TODO: this `as_millis` needs to be kept in sync with the TIMESTAMP_SCALE_NANOS.
                let block_timestamp: i16 = frame_times[packet.input_frameno as usize].as_millis() as i16;
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
}
