//! [`CaptureSource`] on macOS: ScreenCaptureKit for the frames, the window list for where
//! windows are, the accessibility API for resizing and hearing them go, and `NSCursor` for the
//! pointer's picture. Each function is the free function of the same job in this crate.

use slopty_core::WindowId;
use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, WindowInfo};

use crate::source::{
    AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Rect,
    TargetWindow, Went, WindowState,
};
use crate::{Capture, HideWatch, Shareable, Target, geometry};

/// The macOS capture platform.
#[derive(Clone, Copy, Debug)]
pub enum ScreenCaptureKit {}

impl CaptureSource for ScreenCaptureKit {
    type Content = Shareable;
    type HideWatch = HideWatch;
    type Image = slopty_codec::PixelBuffer;
    type Stream = Capture;
    type Target = Target;

    fn can_capture() -> bool {
        geometry::can_capture()
    }

    fn enumerate(done: impl FnOnce(Result<Shareable, CaptureError>) + Send + 'static) {
        crate::enumerate(done);
    }

    fn windows(content: &Shareable) -> Vec<WindowInfo> {
        content.windows()
    }

    fn displays(content: &Shareable) -> Vec<DisplayInfo> {
        content.displays()
    }

    fn resolve(content: &Shareable, kind: CaptureTarget) -> Result<Target, CaptureError> {
        Target::resolve(content, kind)
    }

    fn resolve_crop(content: &Shareable, id: WindowId) -> Result<Option<Target>, CaptureError> {
        Target::resolve_crop(content, id)
    }

    fn crop(target: &Target) -> Option<Crop> {
        target.crop()
    }

    fn pixel_size(target: &Target) -> (u32, u32) {
        target.pixel_size()
    }

    fn point_scale(target: &Target) -> f32 {
        target.point_scale()
    }

    fn start(
        target: &Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        audio: Option<AudioSink>,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Capture, CaptureError> {
        Capture::start(target, config, sink, audio, on_stop, done)
    }

    fn update(
        stream: &Capture,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.update(config, done);
    }

    fn retarget(
        stream: &Capture,
        target: &Target,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.retarget(target, done);
    }

    fn stop(stream: &Capture, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        stream.stop(done);
    }

    fn now_us() -> u64 {
        crate::host_now_us()
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        geometry::target_bounds(target)
    }

    fn refresh_hz(target: CaptureTarget) -> Option<f64> {
        geometry::target_refresh_hz(target)
    }

    fn window_state(id: WindowId) -> Option<WindowState> {
        geometry::window_state(id)
    }

    fn window_bounds(id: WindowId) -> Option<Rect> {
        geometry::window_bounds(id)
    }

    fn window_owner(id: WindowId) -> Option<i32> {
        geometry::window_owner_pid(id)
    }

    fn window_on_screen(id: WindowId) -> bool {
        geometry::window_on_screen(id)
    }

    fn window_title(id: WindowId) -> Option<String> {
        geometry::window_title(id)
    }

    fn occluded(id: WindowId, bounds: &Rect, owner: i32) -> bool {
        geometry::occluded(id, bounds, owner)
    }

    fn display_enclosing(rect: &Rect) -> Option<u32> {
        geometry::display_enclosing(rect)
    }

    fn display_bounds(id: u32) -> Rect {
        geometry::display_bounds(id)
    }

    fn resize_window(
        pid: i32,
        target: &TargetWindow,
        width: f64,
        height: f64,
    ) -> Result<(), AxError> {
        crate::resize_window(pid, target, width, height)
    }

    fn watch_hides(
        pid: i32,
        target: TargetWindow,
        on_went: impl Fn(Went) + Send + Sync + 'static,
    ) -> Result<HideWatch, AxError> {
        HideWatch::start(pid, target, on_went)
    }

    fn watch_targeted(watch: &HideWatch) -> bool {
        watch.targeted()
    }

    fn pointer_moves() -> u32 {
        geometry::pointer_moves()
    }

    fn pointer_location() -> (f64, f64) {
        geometry::pointer_location()
    }

    fn cursor_shape(scale: u8) -> Option<CursorShape> {
        crate::cursor_shape(scale)
    }
}
