// Embeds the Windows executable icon and version metadata. The icon carries
// the lacodda line mark at three levels — the filled tile at the smallest
// sizes, the plated mark above them — so Explorer, the taskbar and the
// titlebar each get the one that reads at their size. See `src/icon.rs`,
// which holds that rule with a test, and `docs/export-assets.mjs`, which
// builds the container.
fn main() {
    // Two conditions, and both are needed.
    //
    // `#[cfg(windows)]` is about the *host*: it decides whether this code is
    // compiled at all, which it must, because `winresource` is a dependency
    // only for the Windows target and naming it elsewhere does not compile.
    //
    // `CARGO_CFG_TARGET_OS` is about the *target*: cross-checking the crate
    // for Linux from a Windows box compiles this file and would otherwise go
    // looking for rc.exe and panic. That is part of why a lint failing only
    // on Linux could not be reproduced locally.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("failed to embed the Windows resources");
    }
}
