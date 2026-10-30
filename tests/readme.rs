//! The README's drivers are `examples/led.rs` and `examples/screen.rs`, which clippy compiles: they
//! must not drift apart.

#[test]
fn the_readme_shows_the_example_drivers() {
    let readme = include_str!("../README.md");
    for (name, example) in
        [("led", include_str!("../examples/led.rs")), ("screen", include_str!("../examples/screen.rs"))]
    {
        let code: String = example.lines().skip(1).map(|line| format!("{line}\n")).collect();
        assert!(readme.contains(&code), "copy examples/{name}.rs (without its first line) into README.md");
    }
}
