//! Cargo.toml owns the version; Git identifies development builds only.

#[path = "build_support/version.rs"]
mod version;

fn main() {
    for path in ["build.rs", "build_support/version.rs", "Cargo.toml"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=LATTICE_RELEASE_VERSION");
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let (revision, watches) = version::repository_metadata(&root);
    for path in watches {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let package = std::env::var("CARGO_PKG_VERSION").unwrap();
    let release = std::env::var("LATTICE_RELEASE_VERSION").ok();
    let display = version::display_version(&package, revision.as_deref(), release.as_deref())
        .expect("invalid release version configuration");
    println!("cargo:rustc-env=LATTICE_VERSION={display}");
}
