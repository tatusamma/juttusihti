//! Embeds the application icon in the Windows executable. The window icon set at run time
//! only covers the title bar; the taskbar and Explorer read the icon from the .exe.

fn main() {
    println!("cargo:rerun-if-changed=../../packaging/icons/juttusihti.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        winresource::WindowsResource::new()
            .set_icon("../../packaging/icons/juttusihti.ico")
            .compile()
            .expect("embedding the Windows icon");
    }
}
