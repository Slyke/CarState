use std::{
    path::Path,
    process::{Command, Output},
};
use styleguide_logger::error_codes::{self, ErrorCodeMap};

fn run(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_error-gen"))
        .arg("--error-file")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn catalog_lifecycle_add_get_search_rename_replace_delete() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/errors.json5");
    success(run(
        &path,
        &[
            "--action",
            "add",
            "--error-key",
            "ONE",
            "--prefix",
            "AB",
            "--deterministic",
        ],
    ));
    let original = error_codes::load_error_codes(&path).unwrap()["ONE"].clone();
    assert!(original.starts_with("AB"));
    assert!(success(run(&path, &["--action", "get", "--error-key", "ONE"])).contains(&original));
    assert!(success(run(&path, &["--action", "search", "--prefix", "ab"])).contains("ONE"));
    success(run(
        &path,
        &[
            "--action",
            "edit",
            "--error-key",
            "ONE",
            "--new-error-key",
            "RENAMED",
        ],
    ));
    let renamed = error_codes::load_error_codes(&path).unwrap();
    assert_eq!(renamed["RENAMED"], original);
    assert!(!renamed.contains_key("ONE"));
    success(run(
        &path,
        &[
            "--action",
            "edit",
            "--error-key",
            "RENAMED",
            "--error-code",
            "111111111111111f",
        ],
    ));
    assert_eq!(
        error_codes::load_error_codes(&path).unwrap()["RENAMED"],
        "111111111111111F"
    );
    success(run(&path, &["--action", "validate"]));
    success(run(
        &path,
        &["--action", "delete", "--error-key", "RENAMED"],
    ));
    assert!(error_codes::load_error_codes(&path).unwrap().is_empty());
}

#[test]
fn read_actions_preserve_comments_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("errors.json5");
    let text = "{\n // Keep this comment\n ERR_UNKNOWN: '0000000000000000',\n}\n";
    std::fs::write(&path, text).unwrap();
    for args in [
        vec!["--action", "get", "--error-key", "ERR_UNKNOWN"],
        vec!["--action", "all"],
        vec!["--action", "search", "--prefix", "0"],
        vec!["--action", "validate"],
    ] {
        success(run(&path, &args));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn invalid_mutations_leave_catalog_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("errors.json5");
    let text = "{A:'0000000000000000',B:'111111111111111F'}";
    std::fs::write(&path, text).unwrap();
    for args in [
        vec!["--action", "add", "--error-key", "A"],
        vec!["--action", "add", "--error-key", "C", "--prefix", "XYZ"],
        vec![
            "--action",
            "edit",
            "--error-key",
            "A",
            "--new-error-key",
            "B",
        ],
        vec![
            "--action",
            "edit",
            "--error-key",
            "A",
            "--error-code",
            "111111111111111F",
        ],
        vec![
            "--action",
            "edit",
            "--error-key",
            "A",
            "--error-code",
            "bad",
        ],
        vec!["--action", "edit", "--error-key", "A"],
        vec!["--action", "delete", "--error-key", "MISSING"],
        vec!["--action", "validate", "--error-code", "bad"],
    ] {
        assert!(!run(&path, &args).status.success(), "{args:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!path.with_file_name("errors.json5.lock").exists());
    }
}

#[test]
fn missing_catalog_is_only_created_by_add() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.json5");
    for action in ["get", "all", "validate", "edit", "delete"] {
        assert!(!run(&path, &["--action", action, "--error-key", "MISSING"])
            .status
            .success());
        assert!(!path.exists());
    }
    success(run(
        &path,
        &["--action", "validate", "--error-code", "0000000000000000"],
    ));
    assert!(!path.exists());
}

#[test]
fn writer_lock_prevents_lost_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("errors.json5");
    let text = "{A:'0000000000000000'}";
    std::fs::write(&path, text).unwrap();
    std::fs::write(dir.path().join("errors.json5.lock"), "").unwrap();
    let output = run(&path, &["--action", "add", "--error-key", "B"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("lock"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn mutations_and_explicit_exports_preserve_environment_references() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.json5");
    let output = dir.path().join("output.json5");
    std::fs::write(&input, "{A:'${TEST_ERROR_CODE}'}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_error-gen"))
        .args([
            "--action",
            "edit",
            "--error-key",
            "A",
            "--new-error-key",
            "B",
            "--error-input-file",
        ])
        .arg(&input)
        .arg("--error-output-file")
        .arg(&output)
        .env("TEST_ERROR_CODE", "0000000000000000")
        .output()
        .unwrap();
    success(result);
    let saved: ErrorCodeMap = json5::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(saved["B"], "${TEST_ERROR_CODE}");
    assert_eq!(
        std::fs::read_to_string(input).unwrap(),
        "{A:'${TEST_ERROR_CODE}'}"
    );
}

#[test]
fn generation_is_reproducible_unique_and_checksummed() {
    // Golden vector for error_gen.js: SHA-256 stable/entropy pieces, count=0, checksum=A.
    assert_eq!(
        error_codes::add_error_code(&mut ErrorCodeMap::new(), "TEST_KEY", "AB", true).unwrap(),
        "AB865668F52C750A"
    );
    let mut first = ErrorCodeMap::new();
    let mut second = ErrorCodeMap::new();
    for prefix in ["", "A", "AB", "ABC", "ABCD"] {
        for i in 0..20 {
            let key = format!("KEY_{prefix}_{i}");
            let a = error_codes::add_error_code(&mut first, &key, prefix, true).unwrap();
            let b = error_codes::add_error_code(&mut second, &key, prefix, true).unwrap();
            assert_eq!(a, b);
            assert!(a.starts_with(prefix));
            assert!(error_codes::validate_error_code(&a));
        }
    }
    error_codes::validate_error_codes(&first).unwrap();
    for i in 0..20 {
        assert!(error_codes::validate_error_code(
            &error_codes::add_error_code(&mut first, &format!("RANDOM_{i}"), "", false).unwrap()
        ));
    }
}

#[cfg(unix)]
#[test]
fn atomic_save_preserves_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("errors.json5");
    std::fs::write(&path, "{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    success(run(&path, &["--action", "add", "--error-key", "A"]));
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}
