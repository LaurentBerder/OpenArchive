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

#[test]
fn test_zip_compression_levels() {
    let test_dir = "test_data_zip_levels";
    fs::create_dir_all(test_dir).unwrap();
    let sample = "The quick brown fox jumps over the lazy dog.\n".repeat(1000);
    fs::write(format!("{}/large.txt", test_dir), &sample).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with level 0 (Store)
    let s0 = Command::new(exe)
        .args(["a", "archive_lvl0.zip", test_dir, "-mx=0"])
        .status()
        .unwrap();
    assert!(s0.success());

    // 2. Create with level 9 (Ultra Deflate)
    let s9 = Command::new(exe)
        .args(["a", "archive_lvl9.zip", test_dir, "-mx=9"])
        .status()
        .unwrap();
    assert!(s9.success());

    let size_0 = fs::metadata("archive_lvl0.zip").unwrap().len();
    let size_9 = fs::metadata("archive_lvl9.zip").unwrap().len();

    // Deflate level 9 must be dramatically smaller than Stored level 0 on repetitive text
    assert!(size_9 < size_0, "level 9 ({}) should be smaller than level 0 ({})", size_9, size_0);

    // Extract level 9 and verify
    let sx = Command::new(exe)
        .args(["x", "archive_lvl9.zip", "-o", "out_zip_lvl9"])
        .status()
        .unwrap();
    assert!(sx.success());

    let extracted = fs::read_to_string("out_zip_lvl9/test_data_zip_levels/large.txt").unwrap();
    assert_eq!(extracted, sample);

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_zip_lvl9");
    let _ = fs::remove_file("archive_lvl0.zip");
    let _ = fs::remove_file("archive_lvl9.zip");
}

#[test]
fn test_7z_compression_options() {
    let test_dir = "test_data_7z_opts";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(format!("{}/f1.txt", test_dir), "File 1 content repeated ".repeat(100)).unwrap();
    fs::write(format!("{}/sub/f2.txt", test_dir), "File 2 content repeated ".repeat(100)).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with -mx=1, -ms=off, -mmt=1
    let s1 = Command::new(exe)
        .args(["a", "archive_fast_nonsolid.7z", test_dir, "-mx=1", "-ms=off", "-mmt=1"])
        .status()
        .unwrap();
    assert!(s1.success());

    // 2. Create with -mx=9, -ms=on, -md=16m, -mmt=on
    let s9 = Command::new(exe)
        .args(["a", "archive_ultra_solid.7z", test_dir, "-mx=9", "-ms=on", "-md=16m", "-mmt=on"])
        .status()
        .unwrap();
    assert!(s9.success());

    // Test both archives
    assert!(Command::new(exe).args(["t", "archive_fast_nonsolid.7z"]).status().unwrap().success());
    assert!(Command::new(exe).args(["t", "archive_ultra_solid.7z"]).status().unwrap().success());

    // Extract solid archive and verify contents
    let sx = Command::new(exe)
        .args(["x", "archive_ultra_solid.7z", "-o", "out_7z_opts"])
        .status()
        .unwrap();
    assert!(sx.success());

    let content_f2 = fs::read_to_string("out_7z_opts/test_data_7z_opts/sub/f2.txt").unwrap();
    assert_eq!(content_f2, "File 2 content repeated ".repeat(100));

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_7z_opts");
    let _ = fs::remove_file("archive_fast_nonsolid.7z");
    let _ = fs::remove_file("archive_ultra_solid.7z");
}
