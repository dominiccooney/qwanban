use anyhow::{anyhow, bail, Context};
use windows::Win32::Foundation::{HWND, ERROR_INVALID_PARAMETER, RECT};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HDC, ReleaseDC, SRCCOPY, SelectObject, HGDIOBJ, HBITMAP};
use windows::Win32::UI::HiDpi::{PROCESS_PER_MONITOR_DPI_AWARE, SetProcessDpiAwareness};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetDesktopWindow,
    GetWindowRect,
};

type ScreenshotImage = image::ImageBuffer<image::Rgba<u8>, Vec<u8>>;

struct DesktopDC {
    hwnd: HWND,
    hdc: HDC,
}

impl DesktopDC {
    unsafe fn get() -> anyhow::Result<DesktopDC> {
        unsafe {
            let hwnd = GetDesktopWindow();
            if hwnd.is_invalid() {
                bail!("failed to get desktop window");
            }
            let hdc = GetDC(Some(hwnd));
            if hdc.is_invalid() {
                ReleaseDC(None, hdc);
                bail!("failed to get screen DC");
            }
            Ok(Self {
                hwnd,
                hdc,
            })
        }
    }
}

impl Drop for DesktopDC {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(Some(self.hwnd), self.hdc);
        }
    }
}

struct OwnedHBITMAP {
    hbitmap: HBITMAP,
}

impl OwnedHBITMAP {
    fn adopt(hbitmap: HBITMAP) -> anyhow::Result<Self> {
        if hbitmap.is_invalid() {
            bail!("bitmap handle is invalid");
        }
        Ok(Self { hbitmap })
    }
}

impl Drop for OwnedHBITMAP {
    fn drop(&mut self) {
        unsafe {
            #[allow(unused_must_use)]
            DeleteObject(self.hbitmap.into());
        }
    }
}

struct OwnedHDC {
    hdc: HDC
}

impl OwnedHDC {
    fn adopt(hdc: HDC) -> anyhow::Result<Self> {
        if hdc.is_invalid() {
            bail!("HDC is invalid");
        }
        Ok(Self {
            hdc
        })
    }
}

impl Drop for OwnedHDC {
    fn drop(&mut self) {
        unsafe {
            #[allow(unused_must_use)]
            DeleteDC(self.hdc);
        }
    }
}

struct DeviceContextBitmapSwitcheroo<'a> {
    hdc: &'a HDC,
    old_bitmap: HGDIOBJ,
}

impl <'a> DeviceContextBitmapSwitcheroo<'a> {
    // Note: The extensive lifetime on new_bitmap is because we will use the bitmap until we
    // swap the old one back, even though we're not holding onto the *handle* anywhere.
    unsafe fn select(hdc: &'a HDC, new_bitmap: &'a HBITMAP) -> Self {
        unsafe {
            let old_bitmap = SelectObject(*hdc, (*new_bitmap).into());
            Self {
                hdc,
                old_bitmap,
            }
        }
    }
}

impl <'a> Drop for DeviceContextBitmapSwitcheroo<'a> {
    fn drop(&mut self) {
        unsafe {
            SelectObject(*self.hdc, self.old_bitmap);
        }
    }
}

pub(crate) fn screenshot() -> anyhow::Result<ScreenshotImage> {
    unsafe {
        #[allow(unused_must_use)]
        SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);

        // Get the desktop and dimensions
        let desktop = DesktopDC::get()?;
        let mut rect = RECT::default();
        GetWindowRect(desktop.hwnd, &mut rect)?;
        let (width, height) = (rect.right, rect.bottom);

        let h_bitmap = OwnedHBITMAP::adopt(CreateCompatibleBitmap(desktop.hdc, width, height)).context("creating bitmap to copy screen contents")?;
        let hdc_memory = OwnedHDC::adopt(CreateCompatibleDC(Some(desktop.hdc))).context("creating memory device context")?;
        let switch = DeviceContextBitmapSwitcheroo::select(&hdc_memory.hdc, &h_bitmap.hbitmap);

        // Copy and extract pixels
        BitBlt(
            hdc_memory.hdc,
            0,
            0,
            width,
            height,
            Some(desktop.hdc),
            rect.left,
            rect.top,
            SRCCOPY,
        )?;
        draw_cursor_to_dc(hdc_memory.hdc, rect.left, rect.top)?;

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
        let get_dib_bits_result = GetDIBits(
            hdc_memory.hdc,
            h_bitmap.hbitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        if get_dib_bits_result == 0 || get_dib_bits_result == ERROR_INVALID_PARAMETER.0 as i32 {
            bail!("failed get bitmap")
        }

        // Clean up
        drop(switch);
        drop(h_bitmap);
        drop(hdc_memory);
        drop(desktop);

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

        let target_x = cursor_info.ptScreenPos.x - screen_x;
        let target_y = cursor_info.ptScreenPos.y - screen_y;

        DrawIconEx(
            hdc,
            target_x,
            target_y,
            cursor_info.hCursor.into(),
            0,
            0,
            0,
            None,
            DI_NORMAL,
        )?;
        Ok(())
    }
}
