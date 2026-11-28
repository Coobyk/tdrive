fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/tdrive.rc");
    println!("cargo:rerun-if-changed=packaging/windows/tdrive.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "windows") {
        embed_resource::compile("packaging/windows/tdrive.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
