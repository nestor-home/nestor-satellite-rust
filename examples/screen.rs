//! The screen of https://nestorhome.dev/docs/sdks/rust/: it shows its device code and takes enrollment
//! codes typed on its keypad.
use nestor_satellite::{AUDIO_PLAYBACK, Device, Display, EnrollmentMethod, IDENTIFY, Manifest, Satellite, TokenStore};

struct Screen;
impl Display for Screen {
    fn show_code(&self, text: &str, code: &str) {
        println!("{text}\n    {code}"); // draw it large, from a task of its own
    }
    fn show_message(&self, text: &str) {
        println!("{text}"); // a refused enrollment code, in the device's language
    }
    fn clear(&self) {
        println!("paired");
    }
}

#[tokio::main]
async fn main() -> nestor_satellite::Result<()> {
    let device = Device::new("ecran-cuisine", "raspberry-pi-zero", "cuisine")?;
    let manifest = Manifest::new(device, [AUDIO_PLAYBACK, IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/ecran-cuisine.json");
    let satellite = Satellite::builder("http://nestor.local:7860", manifest, tokens)
        .display(Screen) // declares device_code.display
        .accepts_enrollment(EnrollmentMethod::Input) // declares enrollment.input
        .build()?;
    let keypad = satellite.enroller();
    std::thread::spawn(move || {
        for typed in std::io::stdin().lines().map_while(Result::ok) {
            if let Err(error) = keypad.enroll(&typed, EnrollmentMethod::Input) {
                eprintln!("{error}"); // not a code, or the device is not waiting for one
            }
        }
    });
    satellite.run().await
}
