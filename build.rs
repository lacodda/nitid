// Embeds the Windows executable icon and version metadata. The icon carries
// the lacodda line mark at three levels — the filled tile at the smallest
// sizes, the plated mark above them — so Explorer, the taskbar and the
// titlebar each get the one that reads at their size. See `src/icon.rs`,
// which holds that rule with a test, and `docs/export-assets.mjs`, which
// builds the container.
fn main() {
    // The *target*, not the host. `#[cfg(windows)]` in a build script asks
    // about the machine doing the building, so cross-checking the crate for
    // Linux from a Windows box went looking for rc.exe and panicked — which
    // is how a lint that only fails on Linux became impossible to reproduce
    // locally.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("failed to embed the Windows resources");
    }
}
