//! The minimal driver of the README and of https://nestorhome.dev/docs/sdks/rust/: a speaker that shows
//! the assistant's attitude.
use nestor_satellite::devices::{Microphone, Speaker};
use nestor_satellite::{
    AUDIO_CAPTURE, AUDIO_PLAYBACK, Behavior, Body, Device, IDENTIFY, Manifest, Satellite, TokenStore,
};

struct Led;
impl Body for Led {
    fn behavior(&self, behavior: Behavior) {
        println!("attitude: {behavior}"); // light the ring, from a task of its own
    }
    fn identify(&self) {
        println!("blink"); // a person looks for this device: blink for a few seconds
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("enceinte-salon", "raspberry-pi-4", "salon")?;
    let manifest = Manifest::new(device, [AUDIO_CAPTURE, AUDIO_PLAYBACK, IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
    let satellite = Satellite::builder("http://nestor.local:7860", manifest, tokens).body(Led);
    satellite.source(Microphone::new()).sink(Speaker::new()).build()?.run().await // pairs once, then reconnects
}
