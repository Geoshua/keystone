//! Frame sources: a built-in calibration pattern and (on Windows) live window
//! capture through the Windows Graphics Capture API.
//!
//! Frames travel from the capture thread to the renderer through `SharedFrame`.
//! Pixels are BGRA8, sRGB-encoded, rows `row_pitch` bytes apart (may be padded).

use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct FrameData {
    pub width: u32,
    pub height: u32,
    pub row_pitch: u32,
    pub bgra: Vec<u8>,
    /// Bumped on every new frame so the renderer only uploads when something changed.
    pub seq: u64,
}

pub type SharedFrame = Arc<Mutex<FrameData>>;

pub fn new_shared() -> SharedFrame {
    Arc::new(Mutex::new(FrameData::default()))
}

/// A capturable window as shown in the source picker.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowEntry {
    pub hwnd: usize,
    pub title: String,
    pub process: String,
}

/// Calibration pattern: checkerboard border, 8x8 grid, centre cross and circles.
/// Circles that look round on the wall mean the warp is right.
pub fn write_test_pattern(frame: &SharedFrame, width: u32, height: u32) {
    let mut px = vec![0u8; (width * height * 4) as usize];
    let (w, h) = (width as f32, height as f32);
    let r_big = h * 0.42;
    let r_small = h * 0.08;
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let i = ((y * width + x) * 4) as usize;
            // Background: dark grey with a subtle gradient so the edges are visible.
            let mut c = [40u8, 40, 40];
            let cell_x = (fx / w * 8.0).fract();
            let cell_y = (fy / h * 8.0).fract();
            let line_w = 2.0 / w * 8.0;
            let line_h = 2.0 / h * 8.0;
            if cell_x < line_w || cell_y < line_h {
                c = [200, 200, 200];
            }
            // Circles centred on the image and in each quadrant.
            let centres = [(w / 2.0, h / 2.0, r_big), (w * 0.125, h * 0.125, r_small), (w * 0.875, h * 0.125, r_small), (w * 0.875, h * 0.875, r_small), (w * 0.125, h * 0.875, r_small)];
            for (cx, cy, r) in centres {
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                if (d - r).abs() < 2.5 {
                    c = [60, 200, 255];
                }
            }
            // Centre cross.
            if ((fx - w / 2.0).abs() < 2.0 && (fy - h / 2.0).abs() < h * 0.05) || ((fy - h / 2.0).abs() < 2.0 && (fx - w / 2.0).abs() < h * 0.05) {
                c = [255, 80, 80];
            }
            // 12 px border, so you can see whether the image reaches the screen edges.
            if fx < 12.0 || fy < 12.0 || fx > w - 12.0 || fy > h - 12.0 {
                c = if ((fx as u32 / 12) + (fy as u32 / 12)) % 2 == 0 { [255, 255, 255] } else { [255, 140, 0] };
            }
            // Stored as BGRA.
            px[i] = c[2];
            px[i + 1] = c[1];
            px[i + 2] = c[0];
            px[i + 3] = 255;
        }
    }
    let mut f = frame.lock().unwrap();
    f.width = width;
    f.height = height;
    f.row_pitch = width * 4;
    f.bgra = px;
    f.seq += 1;
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::{SharedFrame, WindowEntry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
        SecondaryWindowSettings, Settings,
    };
    use windows_capture::window::Window;

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    /// Everything the capture thread needs; passed in through the settings flags.
    pub struct CaptureShared {
        pub frame: SharedFrame,
        pub repaint: eframe::egui::Context,
        pub crop_title_bar: Arc<AtomicBool>,
        pub closed: Arc<AtomicBool>,
    }

    struct Handler {
        shared: CaptureShared,
    }

    impl GraphicsCaptureApiHandler for Handler {
        type Flags = CaptureShared;
        type Error = BoxError;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { shared: ctx.flags })
        }

        fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
            let mut buffer = if self.shared.crop_title_bar.load(Ordering::Relaxed) {
                frame.buffer_without_title_bar()?
            } else {
                frame.buffer()?
            };
            let (width, height, row_pitch) = (buffer.width(), buffer.height(), buffer.row_pitch());
            let raw = buffer.as_raw_buffer();
            // Only the bytes the texture upload will read: full pitch for all but the last row.
            let needed = (row_pitch as usize * (height as usize).saturating_sub(1) + width as usize * 4).min(raw.len());
            {
                let mut f = self.shared.frame.lock().unwrap();
                f.width = width;
                f.height = height;
                f.row_pitch = row_pitch;
                f.bgra.clear();
                f.bgra.extend_from_slice(&raw[..needed]);
                f.seq += 1;
            }
            self.shared.repaint.request_repaint();
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            self.shared.closed.store(true, Ordering::Relaxed);
            self.shared.repaint.request_repaint();
            Ok(())
        }
    }

    pub struct ActiveCapture {
        control: Option<CaptureControl<Handler, BoxError>>,
        pub closed: Arc<AtomicBool>,
    }

    impl ActiveCapture {
        pub fn stop(mut self) {
            if let Some(c) = self.control.take() {
                let _ = c.stop();
            }
        }
    }

    pub fn list_windows() -> Vec<WindowEntry> {
        let own_pid = std::process::id();
        let Ok(windows) = Window::enumerate() else { return Vec::new() };
        let mut out: Vec<WindowEntry> = windows
            .into_iter()
            .filter(|w| w.process_id().map(|p| p != own_pid).unwrap_or(true))
            .filter_map(|w| {
                let title = w.title().ok()?;
                if title.trim().is_empty() {
                    return None;
                }
                Some(WindowEntry {
                    hwnd: w.as_raw_hwnd() as usize,
                    title,
                    process: w.process_name().unwrap_or_default(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.process.to_lowercase().cmp(&b.process.to_lowercase()).then(a.title.cmp(&b.title)));
        out
    }

    pub fn start_capture(entry: &WindowEntry, shared: CaptureShared, show_cursor: bool) -> Result<ActiveCapture, String> {
        let window = Window::from_raw_hwnd(entry.hwnd as *mut std::ffi::c_void);
        if !window.is_valid() {
            return Err("That window no longer exists. Refresh the list.".into());
        }
        let closed = shared.closed.clone();
        let settings = Settings::new(
            window,
            if show_cursor { CursorCaptureSettings::WithCursor } else { CursorCaptureSettings::WithoutCursor },
            // No yellow capture border around the source window.
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            shared,
        );
        let control = Handler::start_free_threaded(settings).map_err(|e| e.to_string())?;
        Ok(ActiveCapture { control: Some(control), closed })
    }
}

#[cfg(not(windows))]
pub use stub::*;

/// Non-Windows builds only offer the calibration pattern (used for development).
#[cfg(not(windows))]
mod stub {
    use super::{SharedFrame, WindowEntry};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[allow(dead_code)] // Mirrors the Windows struct so main.rs compiles unchanged.
    pub struct CaptureShared {
        pub frame: SharedFrame,
        pub repaint: eframe::egui::Context,
        pub crop_title_bar: Arc<AtomicBool>,
        pub closed: Arc<AtomicBool>,
    }

    pub struct ActiveCapture {
        pub closed: Arc<AtomicBool>,
    }

    impl ActiveCapture {
        pub fn stop(self) {}
    }

    pub fn list_windows() -> Vec<WindowEntry> {
        Vec::new()
    }

    pub fn start_capture(_: &WindowEntry, _: CaptureShared, _: bool) -> Result<ActiveCapture, String> {
        Err("Window capture is only available on Windows.".into())
    }
}
