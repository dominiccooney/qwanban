#[cfg(target_os = "windows")]
#[path = "pal/windows/screen.rs"]
mod os_screen_impl;

#[cfg(target_os = "windows")]
#[path = "pal/windows/input.rs"]
mod os_input_impl;

pub(crate) use os_input_impl::*;
pub(crate) use os_screen_impl::*;