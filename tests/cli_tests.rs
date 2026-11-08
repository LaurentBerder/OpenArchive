use std::fs;
use std::process::Command;

#[test]
fn test_roundtrip_7z() {
    let test_dir = "test_data_7z";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(format!("{}/a.txt", test_dir), "File A contents").unwrap();
    fs::write(format!("{}/sub/b.txt", test_dir), "File B contents in subfolder").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create 7z
    let status = Command::new(exe)
        .args(["a", "archive_test.7z", test_dir])
        .status()
        .unwrap();
    assert!(status.success());

    // 2. Test 7z
    let status = Command::new(exe)
        .args(["t", "archive_test.7z"])
        .status()
        .unwrap();
    assert!(status.success());

    // 3. Extract 7z
    let status = Command::new(exe)
        .args(["x", "archive_test.7z", "-o", "out_7z_test"])
        .status()
        .unwrap();
    assert!(status.success());

    let content_a = fs::read_to_string("out_7z_test/test_data_7z/a.txt").unwrap();
    assert_eq!(content_a, "File A contents");

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_7z_test");
    let _ = fs::remove_file("archive_test.7z");
}

#[test]
fn test_roundtrip_zip() {
    let test_dir = "test_data_zip";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(format!("{}/sample.txt", test_dir), "Zip sample content").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create Zip
    let status = Command::new(exe)
        .args(["a", "archive_test.zip", test_dir])
        .status()
        .unwrap();
    assert!(status.success());

    // 2. Test Zip
    let status = Command::new(exe)
        .args(["t", "archive_test.zip"])
        .status()
        .unwrap();
    assert!(status.success());

    // 3. Extract Zip
    let status = Command::new(exe)
        .args(["x", "archive_test.zip", "-o", "out_zip_test"])
        .status()
        .unwrap();
    assert!(status.success());

    let content = fs::read_to_string("out_zip_test/test_data_zip/sample.txt").unwrap();
    assert_eq!(content, "Zip sample content");

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_zip_test");
    let _ = fs::remove_file("archive_test.zip");
}
