//! Tests for the `make update-fennel` command structure.
//!
//! The Makefile target delegates to `scripts/update-fennel.sh`, which is
//! testable without touching the network: the download base and the asset path
//! are environment overrides, and the failure paths never reach the network.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/update-fennel.sh")
}

fn temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "mtrak-update-fennel-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn missing_version_fails_without_touching_the_asset() {
    let dir = temp_dir("missing");
    let asset = dir.join("fennel.lua");
    std::fs::write(&asset, b"original").unwrap();

    let output = Command::new("sh")
        .arg(script())
        .env_remove("FENNEL_VERSION")
        .env("FENNEL_ASSET", &asset)
        .output()
        .unwrap();

    assert!(!output.status.success(), "missing version must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FENNEL_VERSION"), "{stderr}");
    assert_eq!(std::fs::read(&asset).unwrap(), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_download_leaves_the_existing_asset_untouched() {
    let dir = temp_dir("failed");
    let asset = dir.join("fennel.lua");
    std::fs::write(&asset, b"original").unwrap();

    let output = Command::new("sh")
        .arg(script())
        .env("FENNEL_VERSION", "1.6.1")
        // A file URL that cannot exist: the download fails deterministically
        // without any network access.
        .env("FENNEL_URL_BASE", "file:///nonexistent-fennel-downloads")
        .env("FENNEL_ASSET", &asset)
        .output()
        .unwrap();

    assert!(!output.status.success(), "a failed download must fail");
    assert_eq!(std::fs::read(&asset).unwrap(), b"original");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_successful_download_replaces_the_asset() {
    let dir = temp_dir("success");
    let downloads = dir.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(
        downloads.join("fennel-1.6.1.lua"),
        b"-- fake fennel\nreturn {}\n",
    )
    .unwrap();
    let asset = dir.join("assets").join("fennel.lua");
    std::fs::create_dir_all(asset.parent().unwrap()).unwrap();
    std::fs::write(&asset, b"original").unwrap();

    let output = Command::new("sh")
        .arg(script())
        .env("FENNEL_VERSION", "1.6.1")
        .env("FENNEL_URL_BASE", format!("file://{}", downloads.display()))
        .env("FENNEL_ASSET", &asset)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(&asset).unwrap(),
        b"-- fake fennel\nreturn {}\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
