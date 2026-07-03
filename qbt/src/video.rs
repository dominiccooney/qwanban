use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rav1e::prelude::*;
use tokio::sync::{mpsc, Semaphore};
use tokio::task;
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

struct PixelBufferPoolInner {
    free_buffers: Vec<Vec<u8>>,
    buffer_size: usize,
}

struct PixelBufferPool {
    semaphore: Arc<Semaphore>,
    inner: Arc<Mutex<PixelBufferPoolInner>>
}

impl PixelBufferPool {
    const MAX_PIXEL_BUFFER_COUNT: usize = 5;

    pub fn new(mut freed_buffers: mpsc::Receiver<Vec<u8>>, buffer_size: usize) -> Self {
        let semaphore = Arc::new(Semaphore::new(Self::MAX_PIXEL_BUFFER_COUNT));
        let inner = Arc::new(Mutex::new(crate::video::PixelBufferPoolInner {
            free_buffers: Vec::new(),
            buffer_size,
        }));

        // The recycler takes pixel buffers the encoder is finished with and pushes them back on
        // the free list.
        let recycler_semaphore = Arc::clone(&semaphore);
        let recycler_inner = Arc::clone(&inner);
        task::spawn(async move {
            while let Some(buf) = freed_buffers.recv().await {
                assert!(buf.len() >= buffer_size);
                let mut inner = recycler_inner.lock().unwrap();
                inner.free_buffers.push(buf);
                recycler_semaphore.add_permits(1);
            }
        });

        Self {
            semaphore,
            inner,
        }
    }

    async fn get_buffer(&self) -> Vec<u8> {
        let permit = self.semaphore.acquire().await.unwrap();
        permit.forget();

        let mut inner = self.inner.lock().unwrap();
        if let Some(buf) = inner.free_buffers.pop() {
            buf
        } else {
            vec![0; inner.buffer_size]
        }
    }
}

// A frame that we want to submit to the encoder.
struct UnencodedFrame {
    timestamp: Instant,
    pixels_bgra: Vec<u8>,
}

// A command to the encoder thread.
enum EncoderCommand<W: std::io::Write> {
    // Starts writing encoded frames to the writer.
    StartWriting(WebmWriter<W>),
    // Submits a frame. You can submit frames before the writer is available.
    Encode(UnencodedFrame),
    // Tells the encoder to finish encoding and shut down.
    Close,
}

struct ThreadedEncoder<W: std::io::Write> {
    encoder_join: task::JoinHandle<()>,
    cmd_tx: mpsc::Sender<EncoderCommand<W>>,
    pixel_buffer_pool: PixelBufferPool,
}

impl <W: std::io::Write + Send + Sync + 'static> ThreadedEncoder<W> {
    pub(crate) fn new_writing_track_entry(writer: &mut WebmWriter<W>, track_number: u64, (width, height): (usize, usize)) -> anyhow::Result<Self> {
        // Set up a rav1e context
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

        writer.write(&MatroskaSpec::TrackEntry(Master::Start))?;
        writer.write(&MatroskaSpec::TrackNumber(track_number))?;
        writer.write(&MatroskaSpec::TrackUID(track_number))?;
        writer.write(&MatroskaSpec::TrackType(1))?; // 1 = video
        writer.write(&MatroskaSpec::CodecID(String::from("V_AV1")))?;
        writer.write(&MatroskaSpec::CodecPrivate(codec_private))?;
        writer.write(&MatroskaSpec::Video(Master::Start))?;
        writer.write(&MatroskaSpec::PixelWidth(width as u64))?;
        writer.write(&MatroskaSpec::PixelHeight(height as u64))?;
        writer.write(&MatroskaSpec::Video(Master::End))?;
        writer.write(&MatroskaSpec::TrackEntry(Master::End))?;

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<EncoderCommand<W>>(8);
        let (enc_tx, enc_rx) = mpsc::channel::<Vec<u8>>(8);

        let encoder_join = task::spawn_blocking(move || {
            let mut image_yuv = YuvPlanarImageMut::alloc(width as u32, height as u32, YuvChromaSubsampling::Yuv420);
            let mut writer: Option<WebmWriter<W>> = None;

            // Frame times, which we use to calculate durations. Because frames can be
            // generated out of order, it is tricky to prune this collection. Instead,
            // we just let it go. Even at 60 FPS we would only grow ~megabytes per *hour.*
            let mut frame_times: Vec<Instant> = vec![];

            'outer: while let Some(cmd) = cmd_rx.blocking_recv() {
                match cmd {
                    EncoderCommand::StartWriting(mut take_writer) => {
                        // TODO: Check that we are not closed.
                        assert!(writer.is_none());

                        // TODO: Too many unwraps; we should call a helper and handle errors with restart.
                        take_writer.write(&MatroskaSpec::Cluster(Master::Start)).unwrap();
                        take_writer.write(&MatroskaSpec::Timestamp(0)).unwrap();

                        ThreadedEncoder::<W>::write_encoded_frames(&mut encoder_context, track_number, &frame_times, &mut take_writer);

                        writer = Some(take_writer);
                    }
                    EncoderCommand::Encode(unencoded_frame) => {
                        let stride_bgra = (width * 4) as u32;
                        yuvutils_rs::bgra_to_yuv420(&mut image_yuv, &unencoded_frame.pixels_bgra, stride_bgra, YuvRange::Limited, YuvStandardMatrix::Bt709, YuvConversionMode::Fast).unwrap();
                        enc_tx.blocking_send(unencoded_frame.pixels_bgra).unwrap();

                        let mut frame = encoder_context.new_frame();
                        frame.planes[0].copy_from_raw_u8(image_yuv.y_plane.borrow(), image_yuv.y_stride as usize, 1);
                        frame.planes[1].copy_from_raw_u8(image_yuv.u_plane.borrow(), image_yuv.u_stride as usize, 1);
                        frame.planes[2].copy_from_raw_u8(image_yuv.v_plane.borrow(), image_yuv.v_stride as usize, 1);

                        // Subtle: Frame times don't flow through the encoder. We record them to
                        // the side here, and join them with packets as they come out of the
                        // encoder based on frame number.
                        frame_times.push(unencoded_frame.timestamp);

                        match encoder_context.send_frame(frame) {
                            Ok(()) | Err(EncoderStatus::Encoded) => {}
                            Err(EncoderStatus::Failure) => {
                                eprintln!("failed to send frame to encoder");
                                frame_times.pop();
                            }
                            Err(EncoderStatus::EnoughData) => {
                                unreachable!("\"enough data\", but we do not encode frames after flush");
                            }
                            Err(EncoderStatus::LimitReached) => {
                                unreachable!("\"limit reached\", but this is send_frame not receive_packet");
                            }
                            Err(EncoderStatus::NeedMoreData) => {
                                unreachable!("\"need more data\", but this is send_frame not receive_packet");
                            }
                            Err(EncoderStatus::NotReady) => {
                                unreachable!("\"not ready\", but we are not doing two-pass encoding");
                            }
                        }
                        if let Some(writer) = writer.as_mut() {
                            ThreadedEncoder::<W>::write_encoded_frames(&mut encoder_context, track_number, &frame_times, writer);
                        }
                    }
                    EncoderCommand::Close => {
                        drop(enc_tx);
                        encoder_context.flush();

                        match writer.as_mut() {
                            Some(writer) => {
                                ThreadedEncoder::<W>::write_encoded_frames(&mut encoder_context, track_number, &frame_times, writer);

                                writer.write(&MatroskaSpec::Cluster(Master::End)).unwrap();
                                writer.write(&MatroskaSpec::Segment(Master::End)).unwrap();
                                writer.flush().unwrap();
                            }
                            None => {
                                eprintln!("threaded encoder closed without receiving a writer");
                            }
                        }

                        break 'outer
                    }
                }
            }
        });

        Ok(Self {
            encoder_join,
            pixel_buffer_pool: PixelBufferPool::new(enc_rx, width * height * 4),
            cmd_tx,
        })
    }

    pub(crate) async fn encode(&self, time: Instant, frame: Vec<u8>) -> anyhow::Result<()> {
        self.cmd_tx.send(EncoderCommand::Encode(UnencodedFrame { timestamp: time, pixels_bgra: frame })).await?;
        Ok(())
    }

    pub(crate) async fn get_buffer(&mut self) -> Vec<u8> {
        self.pixel_buffer_pool.get_buffer().await
    }

    pub(crate) async fn start_writing(&self, writer: WebmWriter<W>) -> anyhow::Result<()> {
        self.cmd_tx.send(EncoderCommand::StartWriting(writer)).await?;
        Ok(())
    }

    pub(crate) async fn close(self) -> anyhow::Result<()> {
        self.cmd_tx.send(EncoderCommand::Close).await?;
        self.encoder_join.await?;
        Ok(())
    }

    fn write_encoded_frames(encoder_context: &mut rav1e::Context<u16>, track_number: u64, frame_times: &[Instant], writer: &mut WebmWriter<W>) {
        // Pull packets out and mux each one into the Cluster as a SimpleBlock.
        'encoding: loop {
            match encoder_context.receive_packet() {
                Ok(packet) => {
                    // Block timestamps are relative to the enclosing Cluster's
                    // Timestamp, in TimestampScale units, and stored as an i16.
                    let block_timestamp = if packet.input_frameno == 0 { 0i16 } else {
                        // TODO: this `as_millis` needs to be kept in sync with the TIMESTAMP_SCALE_NANOS.
                        // TODO: check this doesn't overflow i16.
                        frame_times[packet.input_frameno as usize].duration_since(frame_times[(packet.input_frameno - 1) as usize]).as_millis() as i16
                    };
                    eprintln!("writing frame #{} duration {}", packet.input_frameno, block_timestamp);
                    let simple_block = SimpleBlock::new_uncheked(
                        &packet.data,
                        track_number,
                        block_timestamp,
                        false,
                        None,
                        false,
                        packet.frame_type == FrameType::KEY,
                    );
                    writer.write(&MatroskaSpec::from(simple_block)).unwrap();
                }
                Err(err) => match err {
                    EncoderStatus::LimitReached | EncoderStatus::NeedMoreData | EncoderStatus::Encoded => {
                        break 'encoding;
                    }
                    EncoderStatus::NotReady => {
                        unreachable!("\"not ready\", but we are not doing two-pass encoding");
                    }
                    EncoderStatus::EnoughData => {
                        unreachable!("\"enough data\", but this is receive_packet not send_frame");
                    }
                    EncoderStatus::Failure => {
                        eprintln!("receive_packet failed");
                        break 'encoding;
                    }
                }
            }
        }
    }
}

pub(crate) async fn encode_video_demo() -> anyhow::Result<()> {
    let sampler = pal::ScreenSampler::new()?;

    let mut writer = WebmWriter::new(File::create("video.webm")?);
    let video_track_number: u64 = 1;

    writer.write(&MatroskaSpec::Ebml(Master::Start))?;
    writer.write(&MatroskaSpec::DocType(String::from("webm")))?;
    writer.write(&MatroskaSpec::Ebml(Master::End))?;

    writer.write(&MatroskaSpec::Segment(Master::Start))?;

    writer.write(&MatroskaSpec::Info(Master::Start))?;
    writer.write(&MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NANOS))?;
    writer.write(&MatroskaSpec::Info(Master::End))?;

    writer.write(&MatroskaSpec::Tracks(Master::Start))?;

    // Video track
    let mut threaded_encoder = ThreadedEncoder::new_writing_track_entry(&mut writer, video_track_number, sampler.size_px())?;

    // TODO: Consider adding an audio track (type 2), e.g. A_OPUS codec

    writer.write(&MatroskaSpec::Tracks(Master::End))?;
    threaded_encoder.start_writing(writer).await?;

    let goal_total_duration = Duration::from_secs(10);
    let goal_delay = Duration::from_secs(1) / 5;
    let start_time = Instant::now();
    while start_time.elapsed() < goal_total_duration {
        // Encode a frame. AV1/rav1e needs planar YUV 4:2:0 data, so convert the
        // interleaved GRBA screen sample first.
        let this_frame_time = Instant::now();
        eprintln!("frame: {:?}", this_frame_time);

        let mut pixels_gbra8 = threaded_encoder.get_buffer().await;
        sampler.sample(&mut pixels_gbra8)?;
        threaded_encoder.encode(this_frame_time, pixels_gbra8).await?;

        let submit_duration = Instant::now().duration_since(this_frame_time);
        if submit_duration < goal_delay {
            tokio::time::sleep(goal_delay - submit_duration).await;
        }
    }

    println!("Closing the threaded encoder.");
    threaded_encoder.close().await?;

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
