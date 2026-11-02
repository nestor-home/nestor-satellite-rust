//! The minimal driver of the README and of https://nestorhome.dev/docs/sdks/rust/: a speaker that shows
//! the assistant's attitude.
use nestor_satellite::devices::{Microphone, Speaker};
use nestor_satellite::{Behavior, Body, Device, IDENTIFY, Manifest, Satellite, TokenStore};

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
    let manifest = Manifest::new(device, [IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/enceinte-salon.json");
    let satellite = Satellite::builder("http://nestor.local:8471", manifest, tokens).body(Led);
    // The microphone declares audio.capture, the speaker audio.playback.
    satellite.source(Microphone::new()).sink(Speaker::new()).build()?.run().await // pairs once, then reconnects
}
