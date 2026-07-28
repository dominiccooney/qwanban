use anyhow::{Context, anyhow, bail};
use core_graphics::display::CGDisplay;

pub(crate) type ScreenshotImage = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

pub(crate) struct ScreenSampler {
    width: usize,
    height: usize,
}

impl ScreenSampler {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let display = CGDisplay::main();
        let mode = display
            .display_mode()
            .ok_or_else(|| anyhow!("Quartz reported no mode for the main display"))?;
        let width = mode.pixel_width() as usize;
        let height = mode.pixel_height() as usize;
        if width == 0 || height == 0 {
            bail!("Quartz reported an empty main display");
        }
        Ok(Self { width, height })
    }

    pub(crate) fn size_px(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub(crate) fn pixel_buffer_size_u8(&self) -> usize {
        self.width * self.height * 4
    }

    pub(crate) fn screenshot(&self) -> anyhow::Result<ScreenshotImage> {
        let mut pixels = vec![0; self.pixel_buffer_size_u8()];
        self.sample(&mut pixels)?;
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        image::RgbaImage::from_raw(self.width as u32, self.height as u32, pixels)
            .ok_or_else(|| anyhow!("failed to create screenshot image"))
    }

    // Gets the main display in the BGRA byte layout shared by the PAL backends.
    pub(crate) fn sample(&self, pixels: &mut [u8]) -> anyhow::Result<()> {
        assert!(pixels.len() >= self.pixel_buffer_size_u8());
        let image = CGDisplay::main().image().ok_or_else(|| {
            anyhow!("Quartz screen capture failed; grant Screen Recording permission to qbt")
        })?;
        if image.width() != self.width || image.height() != self.height {
            bail!(
                "main display changed from {}x{} to {}x{}",
                self.width,
                self.height,
                image.width(),
                image.height()
            );
        }
        if image.bits_per_pixel() != 32 || image.bits_per_component() != 8 {
            bail!(
                "unsupported Quartz screenshot format: {} bits per pixel, {} bits per component",
                image.bits_per_pixel(),
                image.bits_per_component()
            );
        }

        let data = image.data();
        let source = data.bytes();
        let bytes_per_row = image.bytes_per_row();
        source
            .get(..bytes_per_row * self.height)
            .context("Quartz returned a truncated screenshot buffer")?;
        for row in 0..self.height {
            let source_start = row * bytes_per_row;
            let target_start = row * self.width * 4;
            pixels[target_start..target_start + self.width * 4]
                .copy_from_slice(&source[source_start..source_start + self.width * 4]);
        }
        Ok(())
    }
}
