use anyhow::anyhow;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, SRCCOPY};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetWindowRect};

type ScreenshotImage = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

pub(crate) fn screenshot() -> anyhow::Result<ScreenshotImage> {
    unsafe {
        // Get the desktop and dimensions
        let hwnd_desktop = GetDesktopWindow();
        let mut rect = RECT::default();
        GetWindowRect(hwnd_desktop, &mut rect)?;
        assert_eq!(rect.left, 0);
        assert_eq!(rect.top, 0);
        let (width, height) = (rect.right, rect.bottom);

        // Set up the GDI device context
        let hdc_screen = GetDC(Some(hwnd_desktop));
        let hdc_memory = CreateCompatibleDC(Some(hdc_screen));
        let h_bitmap = CreateCompatibleBitmap(hdc_memory, width, height);
        let h_old_bitmap = SelectObject(hdc_memory, h_bitmap.into());

        // Copy and extract pixels
        BitBlt(hdc_memory, 0, 0, width, height, Some(hdc_screen), 0, 0, SRCCOPY)?;

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top down
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        GetDIBits(hdc_memory, h_bitmap, 0, height as u32, Some(pixels.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);

        // Clean up
        SelectObject(hdc_memory, h_old_bitmap);
        if !DeleteObject(h_bitmap.into()).as_bool() {
            anyhow::bail!("failed to delete bitmap");
        }
        if !DeleteDC(hdc_memory).as_bool() {
            anyhow::bail!("failed to delete DC");
        }
        ReleaseDC(hwnd_desktop.into(), hdc_screen);

        // Convert from BGRA to RGBA PNG
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }
        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or(anyhow!("Failed to create image"))
    }
}