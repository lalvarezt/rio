use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, XlibDisplayHandle, XlibWindowHandle,
};
use std::{
    ffi::{c_int, c_ulong, c_void},
    ptr::NonNull,
};
use sugarloaf::{SugarloafWindow, SugarloafWindowSize};

#[link(name = "X11")]
unsafe extern "C" {
    fn XOpenDisplay(name: *const i8) -> *mut c_void;
    fn XDefaultScreen(display: *mut c_void) -> c_int;
    fn XDefaultRootWindow(display: *mut c_void) -> c_ulong;
    fn XCreateSimpleWindow(
        display: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        width: u32,
        height: u32,
        border: u32,
        border_color: c_ulong,
        bg: c_ulong,
    ) -> c_ulong;
    fn XMapWindow(display: *mut c_void, window: c_ulong) -> c_int;
    fn XSync(display: *mut c_void, discard: c_int) -> c_int;
    fn XDestroyWindow(display: *mut c_void, window: c_ulong) -> c_int;
    fn XCloseDisplay(display: *mut c_void) -> c_int;
}

pub struct Window {
    display: *mut c_void,
    window: c_ulong,
}

impl Window {
    pub fn new() -> Self {
        unsafe {
            let display = XOpenDisplay(std::ptr::null());
            assert!(!display.is_null(), "GPU benchmarks need an X11 display");
            let window = XCreateSimpleWindow(
                display,
                XDefaultRootWindow(display),
                0,
                0,
                1536,
                960,
                0,
                0,
                0,
            );
            XMapWindow(display, window);
            XSync(display, 0);
            Self { display, window }
        }
    }

    pub fn handle(&self) -> SugarloafWindow {
        unsafe {
            SugarloafWindow {
                handle: RawWindowHandle::Xlib(XlibWindowHandle::new(self.window)),
                display: RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                    NonNull::new(self.display),
                    XDefaultScreen(self.display),
                )),
                size: SugarloafWindowSize {
                    width: 1536.0,
                    height: 960.0,
                },
                scale: 1.0,
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            XDestroyWindow(self.display, self.window);
            XCloseDisplay(self.display);
        }
    }
}
