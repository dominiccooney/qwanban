use anyhow::{anyhow, bail};
use windows::Win32::Foundation::{ERROR_INVALID_PARAMETER, RECT};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
use windows::Win32::Graphics::Gdi::{BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, SRCCOPY};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetWindowRect, GetCursorInfo, GetSystemMetrics, CURSORINFO, CURSOR_SHOWING, SM_CXCURSOR, SM_CYCURSOR, DI_NORMAL, DrawIconEx};

type ScreenshotImage = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

pub(crate) fn screenshot() -> anyhow::Result<ScreenshotImage> {
    unsafe {
        #[allow(unused_must_use)]
        SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);

        // Get the desktop and dimensions
        let hwnd_desktop = GetDesktopWindow();
        if hwnd_desktop.is_invalid() {
            bail!("failed to get desktop window");
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd_desktop, &mut rect)?;
        let (width, height) = (rect.right, rect.bottom);

        // Set up the GDI device context
        let hdc_screen = GetDC(Some(hwnd_desktop));
        if hdc_screen.is_invalid() {
            ReleaseDC(None, hdc_screen);
            bail!("failed to get screen DC");
        }

        let h_bitmap = CreateCompatibleBitmap(hdc_screen, width, height);
        if h_bitmap.is_invalid() {
            bail!("failed to create bitmap");
        }

        let hdc_memory = CreateCompatibleDC(Some(hdc_screen));
        if hdc_memory.is_invalid() {
            bail!("failed to create memory DC");
        }
        let h_old_bitmap = SelectObject(hdc_memory, h_bitmap.into());

        // Copy and extract pixels
        BitBlt(hdc_memory, 0, 0, width, height, Some(hdc_screen), rect.left, rect.top, SRCCOPY)?;
        draw_cursor_to_dc(hdc_memory, rect.left, rect.top)?;

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let get_dib_bits_result = GetDIBits(hdc_memory, h_bitmap, 0, height as u32, Some(pixels.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
        if get_dib_bits_result == 0 || get_dib_bits_result == ERROR_INVALID_PARAMETER.0 as i32 {
            bail!("failed get bitmap")
        }

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

fn draw_cursor_to_dc(hdc: HDC, screen_x: i32, screen_y: i32) -> anyhow::Result<()> {
    unsafe {
        let mut cursor_info = CURSORINFO {
            cbSize: size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        GetCursorInfo(&mut cursor_info)?;
        if cursor_info.flags != CURSOR_SHOWING {
            return Ok(());
        }

        let cursor_width = GetSystemMetrics(SM_CXCURSOR);
        let cursor_height = GetSystemMetrics(SM_CYCURSOR);
        let target_x = cursor_info.ptScreenPos.x - screen_x;
        let target_y = cursor_info.ptScreenPos.y - screen_y;

        DrawIconEx(hdc, target_x, target_y, cursor_info.hCursor.into(), 0, 0, 0, None, DI_NORMAL)?;
        Ok(())
    }
}