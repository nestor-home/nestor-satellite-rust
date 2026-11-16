//! The camera in sessions and still pictures (feature `video`): one [`Camera`] shared by every session
//! that films and the pictures asked meanwhile, its images encoded in VP8 for WebRTC and in JPEG.
//!
//! [`CameraFeed`] switches the camera on for the first user and off after the last, so it is on only
//! while something is sent; each user gets the latest image, never a backlog.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, warn};
use vp8_encoder::{Encoder, EncoderConfig, Picture};

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

/// The bit rate VP8 aims at.
const BITRATE_BPS: u32 = 1_500_000;
/// The frame rate VP8's rate control assumes; the camera sets the real pace.
const FPS: u32 = 30;

/// Encodes the camera's images in VP8 (RFC 6386: royalty-free, what browsers and the core decode),
/// with a key frame every [`KEY_FRAMES`]. The encoder is made again when the images change size.
pub(crate) struct Vp8 {
    encoder: Option<(Encoder, (u32, u32))>,
    keyed: Option<Instant>,
    planes: [Vec<u8>; 3],
    i420: Vec<u8>,
}

impl Vp8 {
    pub(crate) fn new() -> Self {
        Self { encoder: None, keyed: None, planes: Default::default(), i420: Vec::new() }
    }

    /// One image as a VP8 frame, from its frame tag (empty when rate control skips it).
    pub(crate) fn encode(&mut self, image: &Frame) -> Result<Vec<u8>> {
        let (width, height) = i420(image, &mut self.planes);
        if width == 0 || height == 0 {
            return Err(Error::Video("an image too small to encode".into()));
        }
        let size = (width as u32, height as u32);
        let encoder = match &mut self.encoder {
            Some((encoder, made)) if *made == size => encoder,
            slot => {
                let config = EncoderConfig::new(size.0, size.1, FPS, BITRATE_BPS);
                self.keyed = None; // a new encoder starts with a key frame
                &mut slot.insert((Encoder::new(config).map_err(vp8)?, size)).0
            }
        };
        let now = Instant::now();
        let key = self.keyed.is_some_and(|keyed| now - keyed >= KEY_FRAMES);
        self.i420.clear();
        self.planes.iter().for_each(|plane| self.i420.extend_from_slice(plane));
        let picture =
            Picture::from_i420(&self.i420, size.0, size.1).ok_or(Error::Video("VP8: a short picture".into()))?;
        let Some(frame) = encoder.encode(&picture, key).map_err(vp8)? else { return Ok(Vec::new()) };
        if frame.keyframe || self.keyed.is_none() {
            self.keyed = Some(now);
        }
        Ok(frame.data)
    }
}

fn vp8(error: vp8_encoder::EncodeError) -> Error {
    Error::Video(format!("VP8: {error}"))
}

/// `image` in I420 (BT.601, limited range, as video decoders assume) into `planes`, cropped to even
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

    #[tokio::test(start_paused = true)]
    async fn images_encode_in_vp8_starting_and_then_regularly_with_a_key_frame() {
        let mut encoder = Vp8::new();
        let image = TestPattern::new(64, 48, 30).read().await.unwrap();
        let mut decoder = vp8_decoder::Decoder::new();
        let mut key_frames = Vec::new();
        for _ in 0..30 {
            let frame = encoder.encode(&image).unwrap();
            let info = vp8_decoder::frame_info(&frame).unwrap();
            key_frames.push(info.keyframe);
            let picture = decoder.decode(&frame).unwrap().unwrap();
            assert_eq!((picture.width, picture.height), (64, 48), "a VP8 frame of the image's size");
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        let at: Vec<usize> = key_frames.iter().enumerate().filter(|(_, key)| **key).map(|(i, _)| i).collect();
        assert_eq!(at, [0, 20], "the first, then one every 2 s");
        let odd = Frame::new(63, 47, PixelFormat::Rgb, vec![120; 63 * 47 * 3]).unwrap();
        let resized = encoder.encode(&odd).unwrap();
        assert!(vp8_decoder::frame_info(&resized).unwrap().keyframe, "a new size starts with a key frame");
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
