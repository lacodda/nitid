//! nitid — a fast image viewer for Windows with honest color and HDR.
//!
//! The window and the swapchain belong to this application rather than to a GUI
//! framework: HDR output on Windows is only reachable through `Bt2100Pq` on
//! `Rgb10a2Unorm`, which a framework-managed surface cannot express. See
//! `docs/adr/0001-own-the-swapchain.md`.

fn main() {
    println!("nitid {}", env!("CARGO_PKG_VERSION"));
    println!("Scaffold only — the viewer lands in v0.1.0.");
}
