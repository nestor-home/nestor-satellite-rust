//! The screen of https://nestorhome.dev/docs/sdks/rust/: a kitchen screen with a keyboard, talking with
//! the assistant in writing. It shows its device code, takes enrollment codes typed before it is
//! paired, then sends what is typed and shows the answers. No microphone, no WebRTC: it builds
//! without the `voice` feature.
use nestor_satellite::{Device, Display, EnrollmentMethod, IDENTIFY, Manifest, Satellite, TokenStore};

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
    let manifest = Manifest::new(device, [IDENTIFY])?;
    let tokens = TokenStore::new("~/.config/nestor-satellite/ecran-cuisine.json");
    let satellite = Satellite::builder("http://nestor.local:8471", manifest, tokens)
        .display(Screen) // declares device_code.display
        .accepts_enrollment(EnrollmentMethod::Input) // declares enrollment.input
        .text_display(|text: &str, _last: bool| println!("> {text}")) // declares text.display
        .text_input() // declares text.input
        .build()?;
    let (keypad, keyboard) = (satellite.enroller(), satellite.observer());
    std::thread::spawn(move || {
        for typed in std::io::stdin().lines().map_while(Result::ok) {
            // Before pairing, a line is an enrollment code; once paired, a message to the assistant.
            let sent = if keypad.is_pairing() {
                keypad.enroll(&typed, EnrollmentMethod::Input)
            } else {
                keyboard.input_text(&typed).map(|_connected| ())
            };
            if let Err(error) = sent {
                eprintln!("{error}");
            }
        }
    });
    satellite.run().await
}
