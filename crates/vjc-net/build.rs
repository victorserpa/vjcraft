//! Compiles the vendored Syphon framework (Metal API only) plus a small C shim.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=../../vendor/syphon");
    let on = std::env::var_os("CARGO_FEATURE_SYPHON").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !on {
        return;
    }
    let vendor = PathBuf::from("../../vendor/syphon").canonicalize().unwrap();
    // Public headers import <Syphon/X.h>: expose the vendor dir under that name.
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let inc = out.join("include");
    std::fs::create_dir_all(&inc).unwrap();
    let link = inc.join("Syphon");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&vendor, &link).unwrap();

    let mut b = cc::Build::new();
    b.include(&inc)
        .include(&vendor)
        .flag("-include")
        .flag(vendor.join("Syphon_Prefix.pch").to_str().unwrap())
        .flag("-fobjc-arc")
        .flag("-fmodules")
        .flag("-Wno-deprecated-declarations")
        .flag("-Wno-unused-parameter")
        .warnings(false);
    for entry in std::fs::read_dir(&vendor).unwrap() {
        let p = entry.unwrap().path();
        if matches!(p.extension().and_then(|e| e.to_str()), Some("m" | "c")) {
            b.file(p);
        }
    }
    b.file("csrc/vjc_syphon.m");
    b.compile("vjc_syphon");
    for f in [
        "Foundation",
        "AppKit",
        "Metal",
        "IOSurface",
        "CoreFoundation",
    ] {
        println!("cargo:rustc-link-lib=framework={f}");
    }
}
