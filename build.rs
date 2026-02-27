fn main() {
    // CI sets CLAUSTRE_VERSION (e.g., "version-abc1234"); otherwise derive from git.
    if let Ok(version) = std::env::var("CLAUSTRE_VERSION") {
        println!("cargo:rustc-env=CLAUSTRE_VERSION={version}");
    } else {
        let hash = std::process::Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default()
            .trim()
            .to_string();

        if hash.is_empty() {
            println!("cargo:rustc-env=CLAUSTRE_VERSION=dev");
        } else {
            println!("cargo:rustc-env=CLAUSTRE_VERSION=version-{hash}");
        }
    }
}
