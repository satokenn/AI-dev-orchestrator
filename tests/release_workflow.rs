use std::{fs, path::PathBuf};

fn workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    fs::read_to_string(path).unwrap()
}

#[test]
fn release_is_tag_only_and_builds_both_macos_targets() {
    let yml = workflow();
    assert!(yml.contains("tags:\n      - 'v*'"));
    assert!(yml.contains("target: aarch64-apple-darwin"));
    assert!(yml.contains("target: x86_64-apple-darwin"));
    assert!(!yml.contains("macos-13"));
    assert!(yml.contains(
        "archive=\"ai-dev-orchestrator-${GITHUB_REF_NAME#v}-${{ matrix.target }}.tar.gz\""
    ));
    assert!(yml.contains("cargo build --release --locked --target"));
    assert!(yml.contains("Verify tag matches Cargo version"));
    assert!(yml.contains("test \"$tag_version\" = \"$cargo_version\""));
}

#[test]
fn release_packages_checksum_and_write_permission_is_scoped() {
    let yml = workflow();
    assert!(yml.contains("cp README.md LICENSE"));
    assert!(yml.contains("SHA256SUMS"));
    assert!(yml.contains("shasum -a 256 -c"));
    assert!(yml.contains("gh release create"));
    assert!(yml.contains("permissions:\n      contents: write"));
    assert!(!yml.contains("permissions:\n  contents: write"));
}
