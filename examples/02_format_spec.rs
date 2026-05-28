//! Demonstrates format specifiers supported by ferrilog macros.
//!
//! The framework auto-selects the optimal encoding path based on the
//! format specifier used (e.g., hex uses EncodeConstLowerHexFormat,
//! float precision uses EncodeConstFloatFormat, etc.).
//!
//! Run with:
//!   cargo run --example 02_format_spec

fn main() -> std::io::Result<()> {
    let _guard = ferrilog::builder().level(ferrilog::DBG).start()?;

    ferrilog::set_thread_name("main");

    let value: u32 = 255;
    let pi: f64 = std::f64::consts::PI;

    // --- Hexadecimal ---
    ferrilog::info!("hex lower:       {:x}", value); // ff
    ferrilog::info!("hex lower alt:   {:#x}", value); // 0xff
    ferrilog::info!("hex upper:       {:X}", value); // FF
    ferrilog::info!("hex upper alt:   {:#X}", value); // 0xFF

    // --- Binary ---
    ferrilog::info!("binary:          {:b}", value); // 11111111
    ferrilog::info!("binary alt:      {:#b}", value); // 0b11111111

    // --- Octal ---
    ferrilog::info!("octal:           {:o}", value); // 377
    ferrilog::info!("octal alt:       {:#o}", value); // 0o377

    // --- Floating point precision ---
    ferrilog::info!("pi 2 decimals:   {:.2}", pi); // 3.14
    ferrilog::info!("pi 6 decimals:   {:.6}", pi); // 3.141593

    // --- Width and alignment ---
    // Right-align (default) within 10 characters
    ferrilog::info!("right-aligned:   [{:>10}]", 42u32); // [ 42]
    // Left-align within 10 characters
    ferrilog::info!("left-aligned:    [{:<10}]", 42u32); // [42 ]
    // Center-align within 10 characters
    ferrilog::info!("center-aligned:  [{:^10}]", 42u32); // [    42    ]

    // --- Zero-padding ---
    ferrilog::info!("zero-padded:     [{:0>8}]", 42u32); // [00000042]

    // --- Combined: hex with zero-pad and prefix ---
    ferrilog::info!("hex zero-padded: [{:#010x}]", value); // [0x000000ff]

    Ok(())
}
