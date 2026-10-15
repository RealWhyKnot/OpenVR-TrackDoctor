#[path = "src/channel.rs"]
mod channel;

fn main() {
    println!("cargo:rerun-if-env-changed=TRACKDOCTOR_VERSION");
    println!("cargo:rerun-if-env-changed=TRACKDOCTOR_DEV");
    let version = std::env::var("TRACKDOCTOR_VERSION").ok();
    let forced = std::env::var("TRACKDOCTOR_DEV").ok();
    let dev = channel::is_dev(version.as_deref(), forced.as_deref());
    println!("cargo:rustc-env=TRACKDOCTOR_DEV_BUILD={}", u8::from(dev));
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles)
        .with_debug_info(true);
    slint_build::compile_with_config("ui/app.slint", config).expect("ui compiles");

    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Ok(version) = std::env::var("TRACKDOCTOR_VERSION") {
            res.set("FileVersion", &version);
            res.set("ProductVersion", &version);
        }
        res.set("ProductName", "TrackDoctor");
        res.set("FileDescription", "TrackDoctor");
        res.compile().expect("windows resources compile");
    }
}
