//! Custom types with `#[derive(ferrilog::Encode)]`.
//!
//! To be logged by ferrilog, a custom type must satisfy:
//!   - Copy + 'static + Display
//!   - #[derive(ferrilog::Encode)]
//!
//! The Encode derive generates zero-copy serialization: the struct is
//! memcpy'd into the ring buffer and later decoded via its Display impl.
//!
//! Run with:
//!   cargo run --example 03_custom_type

/// A simple 2D point.
#[derive(Clone, Copy, ferrilog::Encode)]
struct Point {
    x: i32,
    y: i32,
}

impl std::fmt::Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// A trade order with multiple field types.
#[derive(Clone, Copy, ferrilog::Encode)]
struct Order {
    id: u64,
    price: f64,
    qty: u32,
}

impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Order{{id={}, price={:.2}, qty={}}}", self.id, self.price, self.qty)
    }
}

fn main() -> std::io::Result<()> {
    let _guard = ferrilog::builder().level(ferrilog::DBG).start()?;

    ferrilog::set_thread_name("main");

    let p = Point { x: 10, y: -3 };
    let o = Order { id: 100001, price: 49.99, qty: 50 };

    // Log custom types with the default {} placeholder.
    ferrilog::info!("point = {}", p);
    ferrilog::info!("order = {}", o);

    // Format specifiers are applied through the Display trait.
    ferrilog::info!("padded point: [{:^20}]", p);
    ferrilog::info!("padded order: [{:>40}]", o);

    Ok(())
}
