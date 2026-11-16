//! The camera in sessions and still pictures (feature `video`): one [`Camera`] shared by every session
//! that films and the pictures asked meanwhile, its images encoded in H.264 for WebRTC and in JPEG.
//!
//! [`CameraFeed`] switches the camera on for the first user and off after the last, so it is on only
//! while something is sent; each user gets the latest image, never a backlog.

use std::sync::Arc;
use std::time::Duration;

use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate};
use openh264::formats::YUVSlices;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::camera::{Camera, CameraError, Frame, PixelFormat};
use crate::error::{Error, Result};

/// A camera gives its first image within this.
const FIRST_IMAGE: Duration = Duration::from_secs(5);
/// A key frame this often, so that a decoder that lost one does not wait long.
const KEY_FRAMES: Duration = Duration::from_secs(2);
/// The quality of still pictures, out of 100.
const JPEG_QUALITY: u8 = 85;

/// The latest image of the camera, while it is on.
pub(crate) type Images = watch::Receiver<Option<Arc<Frame>>>;

/// One camera for every session that films ([`open`](Self::open) / [`release`](Self::release)) and
/// for still pictures ([`still`](Self::still)).
pub(crate) struct CameraFeed {
    camera: Arc<Mutex<Box<dyn Camera>>>,
    state: Mutex<Feed>,
}

#[derive(Default)]
struct Feed {
    users: usize,
    /// The reader's images, while the camera is on.
    images: Option<Images>,
    reader: Option<JoinHandle<()>>,
}

impl CameraFeed {
    pub(crate) fn new(camera: Box<dyn Camera>) -> Self {
        Self { camera: Arc::new(Mutex::new(camera)), state: Mutex::new(Feed::default()) }
    }

    /// The camera's images for one more user; switches the camera on for the first. Every `open`
    /// that succeeds is followed by a `release`.
    pub(crate) async fn open(&self) -> Result<Images> {
        let mut feed = self.state.lock().await;
        if feed.images.as_ref().is_some_and(|images| images.has_changed().is_err()) {
            debug!("the camera stopped: switching it on again");
            self.switch_off(&mut feed).await;
        }
        if feed.images.is_none() {
            self.camera.lock().await.start().await?;
            let (sender, images) = watch::channel(None);
            let camera = self.camera.clone();
            feed.reader = Some(tokio::spawn(async move {
                loop {
                    let image = camera.lock().await.read().await;
                    match image {
                        Ok(image) => {
                            if sender.send(Some(Arc::new(image))).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            warn!(%error, "the camera stopped");
                            break;
                        }
                    }
                }
            }));
            feed.images = Some(images);
        }
        feed.users += 1;
        feed.images.clone().ok_or(Error::Video("the camera is off".into()))
    }

    /// A user is done; the camera is switched off after the last.
    pub(crate) async fn release(&self) {
        let mut feed = self.state.lock().await;
        feed.users = feed.users.saturating_sub(1);
        if feed.users == 0 {
            self.switch_off(&mut feed).await;
        }
    }

    /// The satellite stops: the camera is switched off, whoever still used it.
    pub(crate) async fn shut(&self) {
        let mut feed = self.state.lock().await;
        feed.users = 0;
        self.switch_off(&mut feed).await;
    }

    async fn switch_off(&self, feed: &mut Feed) {
        if let Some(reader) = feed.reader.take() {
            reader.abort();
            let _ = reader.await;
        }
        if feed.images.take().is_some() {
            self.camera.lock().await.close().await;
        }
    }

    /// A still picture, as JPEG: the camera is on just for it, unless a session films already.
    pub(crate) async fn still(&self) -> Result<Vec<u8>> {
        let mut images = self.open().await?;
        let image = match tokio::time::timeout(FIRST_IMAGE, images.wait_for(Option::is_some)).await {
            Ok(Ok(image)) => Ok(image.clone()),
            Ok(Err(_)) => Ok(None),
            Err(_) => Err(Error::Timeout("no image from the camera")),
        };
        self.release().await;
        let image = image?.ok_or(Error::Camera(CameraError("the camera stopped".into())))?;
        tokio::task::spawn_blocking(move || jpeg(&image)).await.map_err(|e| Error::Video(e.to_string()))?
    }
}

/// A still picture of `image`, as JPEG.
pub(crate) fn jpeg(image: &Frame) -> Result<Vec<u8>> {
    let (width, height) = (dimension(image.width())?, dimension(image.height())?);
    let mut out = Vec::new();
    let encoder = jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY);
    let encoded = match image.format() {
        PixelFormat::Rgb => encoder.encode(image.pixels(), width, height, jpeg_encoder::ColorType::Rgb),
        PixelFormat::Yuyv => {
            let ycbcr: Vec<u8> = image
                .pixels()
                .chunks_exact(4)
                .flat_map(|pair| [pair[0], pair[1], pair[3], pair[2], pair[1], pair[3]])
                .collect();
            encoder.encode(&ycbcr, width, height, jpeg_encoder::ColorType::Ycbcr)
        }
    };
    encoded.map_err(|e| Error::Video(format!("JPEG: {e}")))?;
    Ok(out)
}

fn dimension(pixels: u32) -> Result<u16> {
    u16::try_from(pixels).map_err(|_| Error::Video(format!("{pixels} pixels: too large for a JPEG")))
}

/// Encodes the camera's images in H.264, constrained baseline (what browsers and the core decode),
/// with a key frame every [`KEY_FRAMES`].
pub(crate) struct H264 {
    encoder: Encoder,
    keyed: Option<Instant>,
    planes: [Vec<u8>; 3],
}

impl H264 {
    pub(crate) fn new() -> Result<Self> {
        let config =
            EncoderConfig::new().bitrate(BitRate::from_bps(1_500_000)).max_frame_rate(FrameRate::from_hz(30.0));
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(h264)?;
        Ok(Self { encoder, keyed: None, planes: Default::default() })
    }

    /// One image as an Annex B access unit (empty when the encoder skips it).
    pub(crate) fn encode(&mut self, image: &Frame) -> Result<Vec<u8>> {
        let (width, height) = i420(image, &mut self.planes);
        if width == 0 || height == 0 {
            return Err(Error::Video("an image too small to encode".into()));
        }
        let now = Instant::now();
        if self.keyed.is_some_and(|keyed| now - keyed >= KEY_FRAMES) {
            self.encoder.force_intra_frame();
            self.keyed = Some(now);
        }
        self.keyed.get_or_insert(now);
        let [y, u, v] = &self.planes;
        let source = YUVSlices::new((y, u, v), (width, height), (width, width / 2, width / 2));
        Ok(self.encoder.encode(&source).map_err(h264)?.to_vec())
    }
}

fn h264(error: openh264::Error) -> Error {
    Error::Video(format!("H.264: {error}"))
}

/// `image` in I420 (BT.601, limited range, as H.264 decoders assume) into `planes`, cropped to even
/// dimensions; those dimensions.
fn i420(image: &Frame, planes: &mut [Vec<u8>; 3]) -> (usize, usize) {
    let (width, height) = (image.width() as usize & !1, image.height() as usize & !1);
    let [y, u, v] = planes;
    y.resize(width * height, 0);
    u.resize(width * height / 4, 0);
    v.resize(width * height / 4, 0);
    let stride = image.width() as usize;
    let pixels = image.pixels();
    match image.format() {
        PixelFormat::Rgb => {
            let luma = |r: f32, g: f32, b: f32| 16.0 + (65.738 * r + 129.057 * g + 25.064 * b) / 256.0;
            for row in 0..height {
                for column in 0..width {
                    let at = (row * stride + column) * 3;
                    let [r, g, b] = [pixels[at], pixels[at + 1], pixels[at + 2]].map(f32::from);
                    y[row * width + column] = luma(r, g, b) as u8;
                }
            }
            for row in 0..height / 2 {
                for column in 0..width / 2 {
                    let mut sum = [0.0f32; 3];
                    for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                        let at = ((2 * row + dy) * stride + 2 * column + dx) * 3;
                        for channel in 0..3 {
                            sum[channel] += f32::from(pixels[at + channel]) / 4.0;
                        }
                    }
                    let [r, g, b] = sum;
                    u[row * width / 2 + column] = (128.0 + (-37.945 * r - 74.494 * g + 112.439 * b) / 256.0) as u8;
                    v[row * width / 2 + column] = (128.0 + (112.439 * r - 94.154 * g - 18.285 * b) / 256.0) as u8;
                }
            }
        }
        PixelFormat::Yuyv => {
            for row in 0..height {
                for column in 0..width {
                    y[row * width + column] = pixels[(row * stride + column) * 2];
                }
            }
            for row in 0..height / 2 {
                for column in 0..width / 2 {
                    let (top, bottom) =
                        ((2 * row * stride + 2 * column) * 2, ((2 * row + 1) * stride + 2 * column) * 2);
                    let mean = |a: u8, b: u8| u8::try_from((u16::from(a) + u16::from(b)) / 2).unwrap_or(u8::MAX);
                    u[row * width / 2 + column] = mean(pixels[top + 1], pixels[bottom + 1]);
                    v[row * width / 2 + column] = mean(pixels[top + 3], pixels[bottom + 3]);
                }
            }
        }
    }
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::TestPattern;

    #[tokio::test]
    async fn the_camera_is_on_only_while_someone_uses_it() {
        #[derive(Clone, Default)]
        struct Lens(Arc<std::sync::Mutex<Vec<&'static str>>>);
        #[async_trait::async_trait]
        impl Camera for Lens {
            async fn start(&mut self) -> Result<(), CameraError> {
                self.0.lock().unwrap().push("on");
                Ok(())
            }
            async fn read(&mut self) -> Result<Frame, CameraError> {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Frame::new(4, 2, PixelFormat::Rgb, vec![200; 24])
            }
            async fn close(&mut self) {
                self.0.lock().unwrap().push("off");
            }
        }
        let lens = Lens::default();
        let feed = CameraFeed::new(Box::new(lens.clone()));
        let mut first = feed.open().await.unwrap();
        let _second = feed.open().await.unwrap();
        first.wait_for(Option::is_some).await.unwrap();
        feed.release().await;
        assert_eq!(*lens.0.lock().unwrap(), ["on"], "a session still films");
        assert!(!feed.still().await.unwrap().is_empty());
        feed.release().await;
        assert_eq!(*lens.0.lock().unwrap(), ["on", "off"]);
        feed.still().await.unwrap();
        assert_eq!(*lens.0.lock().unwrap(), ["on", "off", "on", "off"], "on just for the picture");
    }

    #[test]
    fn images_encode_in_h264_with_their_parameters_first() {
        let mut encoder = H264::new().unwrap();
        let image = Frame::new(64, 48, PixelFormat::Rgb, vec![120; 64 * 48 * 3]).unwrap();
        let first = encoder.encode(&image).unwrap();
        let nal_types: Vec<u8> =
            openh264::nal_units(&first).map(|nal| nal[nal.iter().position(|&b| b == 1).unwrap() + 1] & 0x1f).collect();
        assert_eq!(&nal_types[..2], [7, 8], "SPS and PPS before the first picture: {nal_types:?}");
        assert!(nal_types.contains(&5), "the first picture is a key frame");
        let odd = Frame::new(63, 47, PixelFormat::Rgb, vec![120; 63 * 47 * 3]).unwrap();
        assert!(!encoder.encode(&odd).unwrap().is_empty(), "odd sizes are cropped");
    }

    #[tokio::test]
    async fn a_still_picture_is_a_jpeg_of_the_camera_s_size() {
        let feed = CameraFeed::new(Box::new(TestPattern::new(320, 240, 30)));
        let picture = feed.still().await.unwrap();
        assert_eq!(&picture[..2], [0xff, 0xd8], "a JPEG");
        let sof = picture.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
        let size = |at: usize| u16::from_be_bytes([picture[sof + at], picture[sof + at + 1]]);
        assert_eq!((size(7), size(5)), (320, 240));
        let yuyv = Frame::new(2, 2, PixelFormat::Yuyv, vec![76, 85, 76, 255, 76, 85, 76, 255]).unwrap();
        assert_eq!(&jpeg(&yuyv).unwrap()[..2], [0xff, 0xd8]);
    }
}
