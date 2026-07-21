use anyhow::{anyhow, bail, Context};
use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, GetImageReply, ImageFormat, ImageOrder, Screen};
use x11rb::rust_connection::RustConnection;

use crate::pal::x11_connection::{connection, X11Connection};

pub(crate) type ScreenshotImage = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

pub(crate) struct ScreenSampler {
    x11: &'static X11Connection,
    width: usize,
    height: usize,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    msb_first: bool,
}

impl ScreenSampler {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let x11 = connection()?;
        let screen = &x11.screen;
        let (red_mask, green_mask, blue_mask) = visual_masks(screen)
            .context("looking up the root window's visual color masks")?;
        let bits_per_pixel = depth_bits_per_pixel(&x11.conn, screen.root_depth)
            .context("looking up the root window's pixel format")?;
        if bits_per_pixel != 32 {
            bail!("unsupported root window pixel depth: {} bits per pixel (only 32 is supported)", bits_per_pixel);
        }

        Ok(Self {
            x11,
            width: screen.width_in_pixels as usize,
            height: screen.height_in_pixels as usize,
            red_mask,
            green_mask,
            blue_mask,
            msb_first: x11.conn.setup().image_byte_order == ImageOrder::MSB_FIRST,
        })
    }

    pub(crate) fn size_px(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub(crate) fn pixel_buffer_size_u8(&self) -> usize {
        let (width, height) = self.size_px();
        width * height * 4
    }

    // Takes a screenshot and returns an RGBA8 image.
    pub(crate) fn screenshot(&self) -> anyhow::Result<ScreenshotImage> {
        let mut pixels = vec![0u8; self.pixel_buffer_size_u8()];

        self.sample(&mut pixels)?;

        // Convert from BGRA to RGBA PNG
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        let (width, height) = self.size_px();
        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or(anyhow!("Failed to create image"))
    }

    // Gets what's on the screen, in a raw array of BGRA bytes.
    pub(crate) fn sample(&self, pixels: &mut Vec<u8>) -> anyhow::Result<()> {
        let (width, height) = self.size_px();
        assert!(pixels.len() >= self.pixel_buffer_size_u8());

        let reply = get_root_image(&self.x11.conn, self.x11.screen.root, width, height)
            .context("capturing the screen")?;
        decode_zpixmap_to_bgra(&reply.data, self.red_mask, self.green_mask, self.blue_mask, self.msb_first, pixels);
        self.draw_cursor(pixels, width, height).context("compositing the cursor")?;

        Ok(())
    }

    fn draw_cursor(&self, pixels: &mut [u8], width: usize, height: usize) -> anyhow::Result<()> {
        let cursor = self.x11.conn.xfixes_get_cursor_image()?.reply().context("getting the cursor image")?;
        let (cursor_width, cursor_height) = (cursor.width as usize, cursor.height as usize);
        let origin_x = cursor.x as i32 - cursor.xhot as i32;
        let origin_y = cursor.y as i32 - cursor.yhot as i32;

        for row in 0..cursor_height {
            for col in 0..cursor_width {
                let screen_x = origin_x + col as i32;
                let screen_y = origin_y + row as i32;
                if screen_x < 0 || screen_y < 0 || screen_x as usize >= width || screen_y as usize >= height {
                    continue;
                }

                // XFixes cursor pixels are alpha-premultiplied ARGB32 in host byte order; blend
                // them onto the BGRA screen buffer with the standard "over" operator.
                let argb = cursor.cursor_image[row * cursor_width + col];
                let alpha = (argb >> 24) & 0xff;
                if alpha == 0 {
                    continue;
                }
                let src_r = (argb >> 16) & 0xff;
                let src_g = (argb >> 8) & 0xff;
                let src_b = argb & 0xff;
                let inv_alpha = 255 - alpha;

                let offset = (screen_y as usize * width + screen_x as usize) * 4;
                let dst = &mut pixels[offset..offset + 4];
                dst[0] = (src_b + (dst[0] as u32 * inv_alpha) / 255) as u8;
                dst[1] = (src_g + (dst[1] as u32 * inv_alpha) / 255) as u8;
                dst[2] = (src_r + (dst[2] as u32 * inv_alpha) / 255) as u8;
                dst[3] = 255;
            }
        }

        Ok(())
    }
}

fn visual_masks(screen: &Screen) -> anyhow::Result<(u32, u32, u32)> {
    screen.allowed_depths.iter()
        .find(|depth| depth.depth == screen.root_depth)
        .and_then(|depth| depth.visuals.iter().find(|visual| visual.visual_id == screen.root_visual))
        .map(|visual| (visual.red_mask, visual.green_mask, visual.blue_mask))
        .ok_or_else(|| anyhow!("could not find the root visual {} at depth {}", screen.root_visual, screen.root_depth))
}

fn depth_bits_per_pixel(conn: &RustConnection, depth: u8) -> anyhow::Result<u8> {
    conn.setup().pixmap_formats.iter()
        .find(|format| format.depth == depth)
        .map(|format| format.bits_per_pixel)
        .ok_or_else(|| anyhow!("no pixmap format advertised for depth {}", depth))
}

fn get_root_image(conn: &RustConnection, root: u32, width: usize, height: usize) -> anyhow::Result<GetImageReply> {
    Ok(conn.get_image(ImageFormat::Z_PIXMAP, root, 0, 0, width as u16, height as u16, !0u32)?.reply()?)
}

// Decodes ZPixmap data for a 32-bits-per-pixel TrueColor visual into BGRA bytes, matching the
// sample() contract shared with the Windows PAL (see qbt/src/pal/windows/screen.rs). 32-bit
// pixels never need scanline padding, so each pixel maps directly to one output pixel.
fn decode_zpixmap_to_bgra(data: &[u8], red_mask: u32, green_mask: u32, blue_mask: u32, msb_first: bool, out: &mut [u8]) {
    let red_shift = red_mask.trailing_zeros();
    let green_shift = green_mask.trailing_zeros();
    let blue_shift = blue_mask.trailing_zeros();

    for (i, chunk) in data.chunks_exact(4).enumerate() {
        let pixel = if msb_first {
            u32::from_be_bytes(chunk.try_into().unwrap())
        } else {
            u32::from_le_bytes(chunk.try_into().unwrap())
        };
        let r = ((pixel & red_mask) >> red_shift) as u8;
        let g = ((pixel & green_mask) >> green_shift) as u8;
        let b = ((pixel & blue_mask) >> blue_shift) as u8;

        let offset = i * 4;
        out[offset] = b;
        out[offset + 1] = g;
        out[offset + 2] = r;
        out[offset + 3] = 255;
    }
}
