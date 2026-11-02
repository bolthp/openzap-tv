//! Embeds the icon and version info into the Windows executable.
//! Does nothing for other targets.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // The target, not the host: also right when cross-compiling from Linux.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "OpenZap IPTV player");
        res.set("ProductName", "OpenZap");
        if let Err(e) = res.compile() {
            // A missing resource compiler must fail loudly, not ship an exe without icon.
            panic!("cannot embed Windows resources: {e}");
        }
    }
}
