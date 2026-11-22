//! Embeds the app icon and version info into dbclaw.exe on Windows.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("ProductName", "DB Claw");
        res.set("FileDescription", "DB Claw");
        res.compile().expect("embedding the Windows icon");
    }
}
