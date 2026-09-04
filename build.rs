fn main() {
    println!("cargo:rerun-if-changed=assets/weekcase.ico");
    println!("cargo:rerun-if-changed=assets/weekcase.rc");
    embed_resource::compile(
        "assets/weekcase.rc",
        embed_resource::ParamsIncludeDirs(["assets"]),
    )
    .manifest_optional()
    .expect("embed assets/weekcase.ico");
}
