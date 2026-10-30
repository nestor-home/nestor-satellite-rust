//! The README's driver is `examples/led.rs`, which clippy compiles: they must not drift apart.

#[test]
fn the_readme_shows_the_example_driver() {
    let example = include_str!("../examples/led.rs");
    let code: String = example.lines().skip(1).map(|line| format!("{line}\n")).collect();
    assert!(
        include_str!("../README.md").contains(&code),
        "copy examples/led.rs (without its first line) into README.md"
    );
}
