//! The device's camera (feature `video`): filmed only in the sessions a person opens in Nestor, and
//! for the still pictures they ask for.
//!
//! A [`Camera`] gives images; the SDK opens it when the first session or picture needs it and closes
//! it after the last, encodes its images in VP8 for WebRTC and in JPEG for still pictures.
//! `V4l2Camera` (Linux) reads a camera (a USB webcam, the Raspberry Pi camera through its V4L2
//! driver); [`TestPattern`] films colour bars, for tests and for trying a satellite without one.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::time::Instant;

/// A camera failed, or does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("camera: {0}")]
pub struct CameraError(pub String);

/// How an image's pixels are laid out, 8 bits per channel, row after row, no padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PixelFormat {
    /// Red, green, blue: 3 bytes per pixel.
    Rgb,
    /// YUYV 4:2:2, what most webcams give: 2 bytes per pixel, a pair of pixels sharing U and V.
    Yuyv,
}

impl PixelFormat {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Yuyv => 2,
        }
    }
}

/// An image from the camera.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl Frame {
    /// An image of `width` × `height` pixels laid out as `format`; refused when `pixels` does not
    /// hold exactly that, or a YUYV image has an odd width.
    pub fn new(width: u32, height: u32, format: PixelFormat, pixels: Vec<u8>) -> Result<Self, CameraError> {
        let expected = width as usize * height as usize * format.bytes_per_pixel();
        if width == 0 || height == 0 || pixels.len() != expected {
            return Err(CameraError(format!("a {width}x{height} {format:?} image holds {expected} bytes")));
        }
        if format == PixelFormat::Yuyv && width % 2 == 1 {
            return Err(CameraError("a YUYV image has an even width".into()));
        }
        Ok(Self { width, height, format, pixels })
    }

    /// Its width, in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Its height, in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// How its pixels are laid out.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Its pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The colour of the pixel at `x`, `y`, as red, green, blue (BT.601 for YUYV).
    pub fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let (x, y, width) = (x as usize, y as usize, self.width as usize);
        match self.format {
            PixelFormat::Rgb => {
                let at = (y * width + x) * 3;
                [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
            }
            PixelFormat::Yuyv => {
                let pair = (y * width + (x & !1)) * 2;
                let luma = self.pixels[pair + if x % 2 == 0 { 0 } else { 2 }];
                ycbcr_to_rgb(luma, self.pixels[pair + 1], self.pixels[pair + 3])
            }
        }
    }
}

/// Full-range BT.601, as JPEG and webcams use.
fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (f32::from(y), f32::from(cb) - 128.0, f32::from(cr) - 128.0);
    let channel = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    [channel(y + 1.402 * cr), channel(y - 0.344_136 * cb - 0.714_136 * cr), channel(y + 1.772 * cb)]
}

/// Where the satellite's images come from. The SDK calls `start` before the first image a session
/// or a picture needs and `close` after the last, so the camera is on only while something is sent.
#[async_trait]
pub trait Camera: Send + 'static {
    /// Switch the camera on; nothing to do by default.
    async fn start(&mut self) -> Result<(), CameraError> {
        Ok(())
    }

    /// The next image; waits for it (the camera sets the pace).
    async fn read(&mut self) -> Result<Frame, CameraError>;

    /// Switch the camera off.
    async fn close(&mut self) {}
}

/// A camera chosen at run time.
#[async_trait]
impl Camera for Box<dyn Camera> {
    async fn start(&mut self) -> Result<(), CameraError> {
        (**self).start().await
    }

    async fn read(&mut self) -> Result<Frame, CameraError> {
        (**self).read().await
    }

    async fn close(&mut self) {
        (**self).close().await;
    }
}

/// Colour bars sliding by: a camera for tests and for trying a satellite without one.
#[derive(Debug, Clone)]
pub struct TestPattern {
    width: u32,
    height: u32,
    interval: Duration,
    count: u32,
    next: Option<Instant>,
}

/// The colours of the bars, as red, green, blue.
const BARS: [[u8; 3]; 7] =
    [[235, 235, 235], [235, 235, 16], [16, 235, 235], [16, 235, 16], [235, 16, 235], [235, 16, 16], [16, 16, 235]];

impl TestPattern {
    /// Bars of `width` × `height` pixels at `fps` images per second.
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        let interval = Duration::from_secs(1) / fps.max(1);
        Self { width: width.max(2), height: height.max(2), interval, count: 0, next: None }
    }
}

impl Default for TestPattern {
    /// 320 × 240 at 15 images per second.
    fn default() -> Self {
        Self::new(320, 240, 15)
    }
}

#[async_trait]
impl Camera for TestPattern {
    async fn start(&mut self) -> Result<(), CameraError> {
        self.next = None;
        Ok(())
    }

    async fn read(&mut self) -> Result<Frame, CameraError> {
        let now = Instant::now();
        let next = self.next.map_or(now, |next| next + self.interval);
        self.next = Some(next);
        tokio::time::sleep_until(next).await;
        let width = self.width as usize;
        let bar = width.div_ceil(BARS.len());
        let shift = (self.count as usize * 4) % width;
        self.count = self.count.wrapping_add(1);
        let row: Vec<u8> = (0..width).flat_map(|x| BARS[((x + shift) % width) / bar]).collect();
        Frame::new(self.width, self.height, PixelFormat::Rgb, row.repeat(self.height as usize))
    }
}

#[cfg(target_os = "linux")]
pub use v4l2::V4l2Camera;

#[cfg(target_os = "linux")]
mod v4l2 {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use linuxvideo::Device;
    use linuxvideo::format::{PixFormat, PixelFormat as Fourcc};
    use linuxvideo::stream::ReadStream;

    use super::{Camera, CameraError, Frame, PixelFormat};

    /// A Linux camera through V4L2 (`/dev/video0`): a USB webcam, or the Raspberry Pi camera through
    /// its V4L2 driver. It asks for YUYV images of the size given, which the camera may round; a
    /// camera that gives only compressed images (MJPEG) is refused.
    pub struct V4l2Camera {
        path: PathBuf,
        size: (u32, u32),
        stream: Option<Arc<Mutex<Stream>>>,
    }

    struct Stream {
        stream: ReadStream,
        width: u32,
        height: u32,
        bytes_per_line: usize,
    }

    impl V4l2Camera {
        /// The camera at `path` (`/dev/video0`), asked for 640 × 480.
        pub fn new(path: impl Into<PathBuf>) -> Self {
            Self { path: path.into(), size: (640, 480), stream: None }
        }

        /// Ask the camera for images of `width` × `height` pixels.
        pub fn with_size(mut self, width: u32, height: u32) -> Self {
            self.size = (width, height);
            self
        }
    }

    fn failed(what: &str, error: impl std::fmt::Display) -> CameraError {
        CameraError(format!("{what}: {error}"))
    }

    #[async_trait]
    impl Camera for V4l2Camera {
        async fn start(&mut self) -> Result<(), CameraError> {
            let (path, (width, height)) = (self.path.clone(), self.size);
            let opened = tokio::task::spawn_blocking(move || {
                let device = Device::open(&path).map_err(|e| failed(&path.display().to_string(), e))?;
                let capture = device
                    .video_capture(PixFormat::new(width, height, Fourcc::YUYV))
                    .map_err(|e| failed("setting the camera's format", e))?;
                let format = capture.format();
                if format.pixel_format() != Fourcc::YUYV {
                    return Err(CameraError(format!("the camera gives {:?}, not YUYV images", format.pixel_format())));
                }
                let (width, height, bytes_per_line) = (format.width(), format.height(), format.bytes_per_line());
                let stream = capture.into_stream().map_err(|e| failed("starting the camera", e))?;
                Ok(Stream { stream, width, height, bytes_per_line: bytes_per_line as usize })
            })
            .await
            .map_err(|e| failed("opening the camera", e))??;
            self.stream = Some(Arc::new(Mutex::new(opened)));
            Ok(())
        }

        async fn read(&mut self) -> Result<Frame, CameraError> {
            let stream = self.stream.clone().ok_or_else(|| CameraError("the camera is not started".into()))?;
            tokio::task::spawn_blocking(move || {
                let mut stream = stream.lock().map_err(|_| CameraError("the camera is poisoned".into()))?;
                let (width, height, stride) = (stream.width, stream.height, stream.bytes_per_line);
                let row = width as usize * 2;
                let pixels = stream
                    .stream
                    .dequeue(|buffer| {
                        let mut pixels = Vec::with_capacity(row * height as usize);
                        for line in buffer.chunks(stride.max(row)).take(height as usize) {
                            pixels.extend_from_slice(&line[..row.min(line.len())]);
                        }
                        Ok(pixels)
                    })
                    .map_err(|e| failed("reading the camera", e))?;
                Frame::new(width, height, PixelFormat::Yuyv, pixels)
            })
            .await
            .map_err(|e| failed("reading the camera", e))?
        }

        async fn close(&mut self) {
            self.stream = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_holds_exactly_its_pixels() {
        assert!(Frame::new(2, 2, PixelFormat::Rgb, vec![0; 12]).is_ok());
        assert!(Frame::new(2, 2, PixelFormat::Rgb, vec![0; 11]).is_err());
        assert!(Frame::new(2, 1, PixelFormat::Yuyv, vec![0; 4]).is_ok());
        assert!(Frame::new(3, 1, PixelFormat::Yuyv, vec![0; 6]).is_err(), "pairs of pixels");
        assert!(Frame::new(0, 0, PixelFormat::Rgb, vec![]).is_err());
    }

    #[test]
    fn a_yuyv_pixel_reads_as_its_colour() {
        // Pure red in BT.601 full range: Y 76, Cb 85, Cr 255.
        let red = Frame::new(2, 1, PixelFormat::Yuyv, vec![76, 85, 76, 255]).unwrap();
        let [r, g, b] = red.rgb(1, 0);
        assert!(r > 250 && g < 5 && b < 5, "{r} {g} {b}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_test_pattern_films_bars_at_its_pace() {
        let mut camera = TestPattern::new(70, 4, 10);
        camera.start().await.unwrap();
        let started = Instant::now();
        let first = camera.read().await.unwrap();
        camera.read().await.unwrap();
        let third = camera.read().await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_millis(200));
        assert_eq!((first.width(), first.height(), first.rgb(0, 3)), (70, 4, BARS[0]));
        assert_eq!(first.rgb(69, 0), BARS[6]);
        assert_ne!(third.rgb(9, 0), first.rgb(9, 0), "the bars slide");
    }
}
