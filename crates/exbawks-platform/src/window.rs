//! A window showing what the emulated console is drawing.
//!
//! The window runs on its own thread with its own message loop, because a
//! window that is not pumped stops repainting and the operating system
//! declares it hung. The thread owns everything belonging to the window;
//! what crosses between it and the run is a buffer of pixels and two
//! flags, so the run never touches a handle.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A handle the operating system owns.
type Handle = *mut c_void;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_SHOW: i32 = 5;
const PM_REMOVE: u32 = 0x0001;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_QUIT: u32 = 0x0012;
const CW_USEDEFAULT: i32 = i32::MIN;
/// `BI_RGB`: the pixels are stored as they are, uncompressed.
const BI_RGB: u32 = 0;
/// `DIB_RGB_COLORS`: the bitmap's colours are literal, not palette indices.
const DIB_RGB_COLORS: u32 = 0;
/// `SRCCOPY`: the source replaces the destination.
const SRCCOPY: u32 = 0x00CC_0020;

#[repr(C)]
struct WindowClass {
    style: u32,
    procedure: Option<unsafe extern "system" fn(Handle, u32, usize, isize) -> isize>,
    class_extra: i32,
    window_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu: *const u16,
    name: *const u16,
}

#[repr(C)]
#[derive(Default)]
struct Message {
    window: Handle,
    message: u32,
    w: usize,
    l: isize,
    time: u32,
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct BitmapHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    image_size: u32,
    x_pixels_per_meter: i32,
    y_pixels_per_meter: i32,
    colours_used: u32,
    colours_important: u32,
}

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn CreateWindowExW(
        extended: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        parameter: *mut c_void,
    ) -> Handle;
    fn DefWindowProcW(window: Handle, message: u32, w: usize, l: isize) -> isize;
    fn ShowWindow(window: Handle, command: i32) -> i32;
    fn PeekMessageW(
        message: *mut Message,
        window: Handle,
        first: u32,
        last: u32,
        remove: u32,
    ) -> i32;
    fn TranslateMessage(message: *const Message) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(window: Handle) -> i32;
    fn GetDC(window: Handle) -> Handle;
    fn ReleaseDC(window: Handle, context: Handle) -> i32;
    fn GetClientRect(window: Handle, rect: *mut Rect) -> i32;
    fn AdjustWindowRect(rect: *mut Rect, style: u32, menu: i32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn StretchDIBits(
        context: Handle,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        source_x: i32,
        source_y: i32,
        source_width: i32,
        source_height: i32,
        bits: *const c_void,
        info: *const BitmapHeader,
        usage: u32,
        operation: u32,
    ) -> i32;
    fn SetStretchBltMode(context: Handle, mode: i32) -> i32;
}

/// The window procedure: closing the window ends its loop.
///
/// # Safety
///
/// Called by the operating system with a window this module created.
unsafe extern "system" fn procedure(window: Handle, message: u32, w: usize, l: isize) -> isize {
    match message {
        WM_CLOSE => {
            // SAFETY: `window` is the operating system's own handle for a
            // live window, passed to this procedure for exactly this.
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            // SAFETY: posting a quit message takes no pointer and affects
            // only the calling thread's message queue.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: the default handler is being given back arguments it
        // supplied, which is what every unhandled message expects.
        _ => unsafe { DefWindowProcW(window, message, w, l) },
    }
}

/// The picture the run has produced, shared with the window's thread.
#[derive(Debug, Default)]
struct Picture {
    width: u32,
    height: u32,
    /// Rows top to bottom, four bytes per pixel, blue first.
    pixels: Vec<u8>,
}

/// A window showing frames a run hands it.
#[derive(Debug)]
pub struct Display {
    picture: Arc<Mutex<Picture>>,
    open: Arc<AtomicBool>,
}

impl Display {
    /// Opens a window of the given size and starts pumping it.
    ///
    /// Returns `None` when the window cannot be created, which a run
    /// should treat as "no display" rather than as a failure.
    #[must_use]
    pub fn open(title: &str, width: u32, height: u32) -> Option<Self> {
        let picture = Arc::new(Mutex::new(Picture::default()));
        let open = Arc::new(AtomicBool::new(true));
        let (drawing, running) = (Arc::clone(&picture), Arc::clone(&open));

        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let class: Vec<u16> = "exbawks".encode_utf16().chain(std::iter::once(0)).collect();
        std::thread::Builder::new()
            .name("display".to_owned())
            .spawn(move || pump(&title, &class, width, height, &drawing, &running))
            .ok()?;
        Some(Self { picture, open })
    }

    /// Whether the window is still open. A run watches this so closing the
    /// window stops it.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Hands over a frame to show, as rows of 8-bit RGBA.
    ///
    /// The pixels are converted here rather than on the window's thread so
    /// the run pays for its own frame.
    pub fn present(&self, width: u32, height: u32, rgba: &[u8]) {
        let expected = (width as usize).saturating_mul(height as usize).saturating_mul(4);
        if expected == 0 || rgba.len() < expected {
            return;
        }
        let Ok(mut picture) = self.picture.lock() else {
            return;
        };
        picture.width = width;
        picture.height = height;
        picture.pixels.clear();
        picture.pixels.reserve(expected);
        // The bitmap wants blue first and the alpha byte ignored.
        for pixel in rgba[..expected].chunks_exact(4) {
            picture.pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0]);
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.open.store(false, Ordering::Relaxed);
    }
}

/// Creates the window and pumps it until it closes.
fn pump(
    title: &[u16],
    class: &[u16],
    width: u32,
    height: u32,
    picture: &Arc<Mutex<Picture>>,
    running: &Arc<AtomicBool>,
) {
    let description = WindowClass {
        style: 0,
        procedure: Some(procedure),
        class_extra: 0,
        window_extra: 0,
        instance: std::ptr::null_mut(),
        icon: std::ptr::null_mut(),
        cursor: std::ptr::null_mut(),
        background: std::ptr::null_mut(),
        menu: std::ptr::null(),
        name: class.as_ptr(),
    };
    // SAFETY: `description` and the name it points at outlive this call,
    // and registering a class that already exists is reported rather than
    // being an error this cares about.
    unsafe { RegisterClassW(&raw const description) };

    // The requested size is the drawable area, so the frame is added on.
    let mut rect = Rect { left: 0, top: 0, right: width as i32, bottom: height as i32 };
    // SAFETY: `rect` is a local the callee only writes.
    unsafe { AdjustWindowRect(&raw mut rect, WS_OVERLAPPEDWINDOW, 0) };

    // SAFETY: the class was registered above and both strings are
    // nul-terminated and outlive the call; every handle argument is null
    // because the window has no parent, menu, or creation parameter.
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if window.is_null() {
        running.store(false, Ordering::Relaxed);
        return;
    }
    // SAFETY: `window` was just created and is live.
    unsafe { ShowWindow(window, SW_SHOW) };

    while running.load(Ordering::Relaxed) {
        let mut message = Message::default();
        // SAFETY: `message` is a local the callee fills; a null window asks
        // for every message belonging to this thread.
        while unsafe { PeekMessageW(&raw mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0
        {
            if message.message == WM_QUIT {
                running.store(false, Ordering::Relaxed);
                break;
            }
            // SAFETY: `message` was filled by the call above and is a local
            // that outlives both calls.
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        if !running.load(Ordering::Relaxed) {
            break;
        }
        paint(window, picture);
        // A frame every sixteen milliseconds is as often as a screen
        // changes; pumping faster would only spend the run's processor.
        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    // SAFETY: `window` is still this thread's to destroy, and destroying
    // it once here is the last use of it.
    unsafe { DestroyWindow(window) };
    running.store(false, Ordering::Relaxed);
}

/// Draws the latest picture into the window, scaled to its client area.
fn paint(window: Handle, picture: &Arc<Mutex<Picture>>) {
    let Ok(picture) = picture.lock() else {
        return;
    };
    if picture.pixels.is_empty() {
        return;
    }
    let mut rect = Rect::default();
    // SAFETY: `window` is live and `rect` is a local the callee writes.
    unsafe { GetClientRect(window, &raw mut rect) };
    let (client_width, client_height) = (rect.right - rect.left, rect.bottom - rect.top);
    if client_width <= 0 || client_height <= 0 {
        return;
    }

    let header = BitmapHeader {
        size: u32::try_from(size_of::<BitmapHeader>()).unwrap_or(0),
        width: picture.width as i32,
        // Negative means the rows run top to bottom, as they are held.
        height: -(picture.height as i32),
        planes: 1,
        bit_count: 32,
        compression: BI_RGB,
        ..BitmapHeader::default()
    };

    // SAFETY: `window` is live, and the context is released below on every
    // path out of this function.
    let context = unsafe { GetDC(window) };
    if context.is_null() {
        return;
    }
    // SAFETY: the context is live; `header` and the pixel buffer are held
    // by this frame and describe each other — the header's dimensions are
    // the buffer's, at four bytes a pixel.
    unsafe {
        // Halftone scaling, so an enlarged picture is smoothed.
        SetStretchBltMode(context, 4);
        StretchDIBits(
            context,
            0,
            0,
            client_width,
            client_height,
            0,
            0,
            picture.width as i32,
            picture.height as i32,
            picture.pixels.as_ptr().cast::<c_void>(),
            &raw const header,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        ReleaseDC(window, context);
    }
}
