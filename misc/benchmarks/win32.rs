use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
};
use std::{ffi::c_void, num::NonZeroIsize};
use sugarloaf::{SugarloafWindow, SugarloafWindowSize};

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowExW(
        extended_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: *mut c_void,
        menu: *mut c_void,
        instance: *mut c_void,
        parameter: *mut c_void,
    ) -> *mut c_void;
    fn DestroyWindow(window: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

pub struct Window {
    window: *mut c_void,
    instance: *mut c_void,
}

impl Window {
    pub fn new() -> Self {
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let instance = GetModuleHandleW(std::ptr::null());
            // No visible surface is needed: the fixture renders to its own texture.
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0x80000000,
                0,
                0,
                1536,
                960,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            assert!(!window.is_null(), "Cannot create the benchmark HWND");
            Self { window, instance }
        }
    }

    pub fn handle(&self) -> SugarloafWindow {
        let mut handle =
            Win32WindowHandle::new(NonZeroIsize::new(self.window as isize).unwrap());
        handle.hinstance = NonZeroIsize::new(self.instance as isize);
        SugarloafWindow {
            handle: RawWindowHandle::Win32(handle),
            display: RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
            size: SugarloafWindowSize {
                width: 1536.0,
                height: 960.0,
            },
            scale: 1.0,
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
