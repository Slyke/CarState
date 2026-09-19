use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=BUILD_HASH");
    println!("cargo:rerun-if-env-changed=BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=CARSTATE_RELEASE");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    let hash = env::var("BUILD_HASH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".into());
    if env::var("CARSTATE_RELEASE").as_deref() == Ok("true") {
        assert!(
            hash.len() >= 12 && hash.len() <= 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "Release requires a real BUILD_HASH"
        );
    }
    let version = env::var("BUILD_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap());
    let mut info = serde_json::json!({"version": version, "buildHash": hash});
    if let Some(number) = env::var("BUILD_NUMBER").ok().filter(|s| !s.is_empty()) {
        info["buildNumber"] = number.into();
    }
    let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("build-info.json");
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&info).unwrap()),
    )
    .unwrap();
}
