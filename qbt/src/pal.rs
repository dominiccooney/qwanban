#[cfg(target_os = "windows")]
#[path = "pal/windows/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "windows")]
#[path = "pal/windows/input.rs"]
mod os_input_impl;

#[cfg(target_os = "linux")]
#[path = "pal/x11/connection.rs"]
mod x11_connection;

#[cfg(target_os = "linux")]
#[path = "pal/x11/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "linux")]
#[path = "pal/x11/input.rs"]
mod os_input_impl;

pub(crate) use os_input_impl::*;
pub(crate) use os_screen_impl::*;
