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
                bail!("failed to get desktop device context");
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

// Takes ownership of a HDC and a HBITMAP, switching the HDC's bitmap for the lifetime of this
// Switcheroo.
struct DeviceContextBitmapSwitcheroo {
    hdc: OwnedHDC,
    old_bitmap: HGDIOBJ,
    bitmap: OwnedHBITMAP,
}

impl DeviceContextBitmapSwitcheroo {
    unsafe fn select(hdc: OwnedHDC, new_bitmap: OwnedHBITMAP) -> Self {
        unsafe {
            let old_bitmap = SelectObject(hdc.hdc, new_bitmap.hbitmap.into());
            Self {
                hdc,
                old_bitmap,
                bitmap: new_bitmap,
            }
        }
    }
}

impl Drop for DeviceContextBitmapSwitcheroo {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc.hdc, self.old_bitmap);
        }
    }
}

pub(crate) struct ScreenSampler {
    desktop: DesktopDC,
    rect: RECT,
    switch: DeviceContextBitmapSwitcheroo,
}

impl ScreenSampler {
    pub(crate) fn new() -> anyhow::Result<Self> {
        unsafe {
            #[allow(unused_must_use)]
            SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);

            // Get the desktop and dimensions
            let desktop = DesktopDC::get()?;
            let mut rect = RECT::default();
            GetWindowRect(desktop.hwnd, &mut rect)?;
            let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);

            let h_bitmap = OwnedHBITMAP::adopt(CreateCompatibleBitmap(desktop.hdc, width, height)).context("creating bitmap to copy screen contents")?;
            let hdc_memory = OwnedHDC::adopt(CreateCompatibleDC(Some(desktop.hdc))).context("creating memory device context")?;
            let switch = DeviceContextBitmapSwitcheroo::select(hdc_memory, h_bitmap);

            Ok(Self {
                desktop,
                rect,
                switch,
            })
        }
    }

    // Takes a screenshot and returns an RGBA8 image.
    pub(crate) fn screenshot(&self) -> anyhow::Result<ScreenshotImage> {
        let (width, height) = (self.rect.right - self.rect.left, self.rect.bottom - self.rect.top);
        let mut pixels = vec![0u8; (width * height * 4) as usize];

        self.screen_sample(&mut pixels)?;

        // Convert from BGRA to RGBA PNG
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or(anyhow!("Failed to create image"))
    }

    // Gets what's on the screen, in a raw array of BGRA bytes.
    pub(crate) fn screen_sample(&self, pixels: &mut Vec<u8>) -> anyhow::Result<()> {
        unsafe {
            let (width, height) = (self.rect.right - self.rect.left, self.rect.bottom - self.rect.top);

            // Copy and extract pixels
            BitBlt(
                self.switch.hdc.hdc,
                0,
                0,
                width,
                height,
                Some(self.desktop.hdc),
                self.rect.left,
                self.rect.top,
                SRCCOPY,
            )?;
            draw_cursor_to_dc(self.switch.hdc.hdc, self.rect.left, self.rect.top)?;

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

            assert!((width * height * 4) as usize <= pixels.len());
            let get_dib_bits_result = GetDIBits(
                self.switch.hdc.hdc,
                self.switch.bitmap.hbitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if get_dib_bits_result == 0 || get_dib_bits_result == ERROR_INVALID_PARAMETER.0 as i32 {
                bail!("failed get bitmap")
            }

            Ok(())
        }
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
