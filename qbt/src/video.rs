use std::fs::File;
use std::io::Write;
use std::num::NonZero;
use std::ops::Sub;
use std::time::Duration;
use tokio::time::Instant;
use vpx_rs::{CompressedFrame, Encoder, EncoderConfig, EncoderFrameFlags, ImageFormat, Packet, RateControl, Timebase};
use webm::mux::VideoCodecId;
use yuvutils_rs::{BufferStoreMut, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix};
use crate::pal;

struct UnencodedFrame {
    pixels_yuv420: Vec<u8>,
    timestamp: Instant,
}

trait FrameOutputter {
    fn process(&mut self, frame: CompressedFrame, timestamp: Duration) -> anyhow::Result<()>;
}

impl<F> FrameOutputter for F
where
    F: FnMut(CompressedFrame, Duration) -> anyhow::Result<()>
{
    fn process(&mut self, frame: CompressedFrame, timestamp: Duration) -> anyhow::Result<()> {
        self(frame, timestamp)
    }
}

/// Captures and encodes a screen recording with low CPU utilization, for later offline re-encoding.
struct VideoCapturer<W: FrameOutputter> {
    frame_writer: W,
    encoder: Encoder<u8>,
    width: usize,
    height: usize,
    start_time: Option<Instant>,
    previous_frame: Option<UnencodedFrame>,
}

impl <W: FrameOutputter> VideoCapturer<W> {
    pub(crate) fn new((width, height): (usize, usize), writer: W) -> anyhow::Result<Self> {
        let config = EncoderConfig::<u8>::new(
            vpx_rs::enc::CodecId::VP9,
            width as u32,
            height as u32,
            // Set the timebase to 1ms, even though we won't be encoding 1_000 FPS, so we can
            // use millisecond timestamps as presentation times.
            Timebase {
                num: NonZero::new(1).unwrap(),
                den: NonZero::new(Duration::from_secs(1).as_millis() as u32).unwrap(),
            },
            // This is faster than constant bit-rate but produces larger files.
            RateControl::Lossless,
        )?;

        Ok(Self {
            frame_writer: writer,
            encoder: Encoder::<u8>::new(config)?,
            width,
            height,
            start_time: None,
            previous_frame: None,
        })
    }

    pub(crate) fn flush(&mut self) -> anyhow::Result<()> {
        if let Some(previous_frame) = self.previous_frame.take() {
            // We present the last frame for an arbitrary 32 msec (= 30 FPS.)
            let presentation_end = previous_frame.timestamp + Duration::from_millis(32);
            self.encode_and_write(previous_frame, presentation_end)?;
        }
        Ok(())
    }

    pub(crate) fn encode(&mut self, pixels_yuv420: Vec<u8>, timestamp: Instant) -> anyhow::Result<()> {
        if let Some(previous_frame) = self.previous_frame.take() {
            self.encode_and_write(previous_frame, timestamp)?;
        } else if self.start_time.is_none() {
            self.start_time = Some(timestamp);
        }
        self.previous_frame = Some(UnencodedFrame {
            pixels_yuv420,
            timestamp,
        });
        Ok(())
    }

    fn encode_and_write(&mut self, frame: UnencodedFrame, end_timestamp: Instant) -> anyhow::Result<()> {
        let image = vpx_rs::YUVImageData::<u8>::from_raw_data(
            ImageFormat::I420,
            self.width,
            self.height,
            &frame.pixels_yuv420,
        )?;
        let packets = self.encoder.encode(
            frame.timestamp.duration_since(self.start_time.unwrap()).as_millis() as i64,
            end_timestamp.duration_since(frame.timestamp).as_millis() as u64,
            image,
            vpx_rs::EncodingDeadline::Realtime,
            EncoderFrameFlags::empty(),
        )?;
        for packet in packets {
            match packet {
                Packet::CompressedFrame(frame) => {
                    let frame_presentation_time = Duration::from_millis(frame.pts as u64);
                    self.frame_writer.process(frame, frame_presentation_time)?;
                }
                _ => {
                    unreachable!("encoder is not configured to produce packet: {:?}", packet);
                }
            }
        }
        Ok(())
    }
}

pub(crate) async fn offline_encode_video_demo() -> anyhow::Result<()> {
    let sampler = pal::ScreenSampler::new()?;

    let file = File::create("video.webm")?;
    let writer = webm::mux::Writer::new(file);

    let builder = webm::mux::SegmentBuilder::new(writer)?.set_mode(webm::mux::SegmentMode::File);
    let (width, height) = sampler.size_px();
    let (builder, video_track) = builder?.add_video_track(width as u32, height as u32, VideoCodecId::VP9, None)?;
    let mut segment = builder.build();

    let mut encoder = VideoCapturer::new(sampler.size_px(), |compressed_frame: CompressedFrame, timestamp: Duration| {
        segment.add_frame(video_track, &compressed_frame.data, timestamp.as_nanos() as u64, compressed_frame.flags.is_key)?;
        Ok(())
    })?;

    let mut pixels_bgra = vec![0u8; width * height * 4];

    let goal_total_duration = Duration::from_secs(15);
    let goal_delay = Duration::from_secs(1) / 15;
    let start_time = Instant::now();
    while start_time.elapsed() < goal_total_duration {
        let this_frame_time = Instant::now();
        sampler.sample(&mut pixels_bgra)?;

        let mut pixels_yuv420 = vec![0u8; ImageFormat::I420.buffer_len(width, height)?];
        let (pixels_y, pixels_uv) = pixels_yuv420.split_at_mut(width * height);
        let (pixels_u, pixels_v) = pixels_uv.split_at_mut(width / 2 * height / 2);
        let mut image_yuv420 = YuvPlanarImageMut {
            width: width as u32,
            height: height as u32,
            y_plane: BufferStoreMut::Borrowed(pixels_y),
            y_stride: width as u32,
            u_plane: BufferStoreMut::Borrowed(pixels_u),
            u_stride: (width / 2) as u32,
            v_plane: BufferStoreMut::Borrowed(pixels_v),
            v_stride: (width / 2) as u32,
        };
        let stride_bgra = (width * 4) as u32;
        yuvutils_rs::bgra_to_yuv420(&mut image_yuv420, &pixels_bgra, stride_bgra, YuvRange::Limited, YuvStandardMatrix::Bt709, YuvConversionMode::Fast)?;

        encoder.encode(pixels_yuv420, this_frame_time)?;
        let submit_duration = Instant::now().duration_since(this_frame_time);
        if submit_duration < goal_delay {
            eprintln!("duration: {}", submit_duration.as_millis());
            tokio::time::sleep(goal_delay - submit_duration).await;
        } else {
            eprintln!("late! {}", submit_duration.sub(goal_delay).as_millis());
        }
    }

    println!("Closing the encoder.");
    encoder.flush()?;
    let writer = segment.finalize(None).unwrap_or_else(|writer| {
        eprintln!("could not finalize segment");
        writer
    });
    writer.into_inner().flush()?;

    Ok(())
}