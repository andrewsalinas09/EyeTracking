// Fingerprint the compiled sources so recorded datasets identify the exact build,
// including uncommitted edits; package versions alone cannot reproduce a model.
use std::{fs, path::Path, process::Command};
fn collect(p: &Path, files: &mut Vec<std::path::PathBuf>) {
    for e in fs::read_dir(p).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, files)
        } else {
            files.push(p)
        }
    }
}
fn main() {
    let mut files = vec!["Cargo.toml".into(), "Cargo.lock".into(), "build.rs".into()];
    collect(Path::new("src"), &mut files);
    files.sort();
    let mut hash = 0xcbf29ce484222325u64;
    for p in files {
        println!("cargo:rerun-if-changed={}", p.display());
        for b in p.to_string_lossy().bytes().chain(fs::read(&p).unwrap()) {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    println!("cargo:rustc-env=EYETRACKING_SOURCE=fnv1a64:{hash:016x}");
    let rev = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=EYETRACKING_GIT={rev}");
}
