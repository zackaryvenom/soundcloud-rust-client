fn main() {
    println!("cargo:rerun-if-changed=assets/rustcloud.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resources = winres::WindowsResource::new();
    resources.set_icon("assets/rustcloud.ico");
    resources
        .compile()
        .expect("Failed to embed the RustCloud application icon");
}
