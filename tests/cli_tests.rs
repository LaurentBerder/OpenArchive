use std::fs;
use std::path::PathBuf;
use std::process::Command;

use sevenz_rust2::Archive;

/// LZMA2 coder properties encode the dictionary size, so comparing them proves
/// that `-mx` presets and `-md` overrides actually reach the encoder.
fn lzma2_properties(path: &str) -> Vec<u8> {
    let archive = Archive::open(path).unwrap();
    archive
        .blocks
        .iter()
        .flat_map(|block| block.coders.iter())
        .find(|coder| coder.encoder_method_id() == sevenz_rust2::EncoderMethod::ID_LZMA2)
        .map(|coder| coder.properties().to_vec())
        .expect("archive must contain an LZMA2 coder")
}

fn has_coder(path: &str, method_id: &[u8]) -> bool {
    let archive = Archive::open(path).unwrap();
    archive
        .blocks
        .iter()
        .flat_map(|block| block.coders.iter())
        .any(|coder| coder.encoder_method_id() == method_id)
}

#[test]
fn test_roundtrip_7z() {
    let test_dir = "test_data_7z";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(format!("{}/a.txt", test_dir), "File A contents").unwrap();
    fs::write(
        format!("{}/sub/b.txt", test_dir),
        "File B contents in subfolder",
    )
    .unwrap();

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
    // Varied, moderately compressible data makes Deflate's level differences
    // observable; a single repeated line can compress to the same size at many
    // levels.
    let mut sample = String::with_capacity(1024 * 1024);
    for i in 0..20_000 {
        sample.push_str(&format!(
            "line {i}: the quick brown fox jumps over the lazy dog {}\n",
            (i * 7919) % 100_003
        ));
    }
    fs::write(format!("{}/large.txt", test_dir), &sample).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with level 0 (Store)
    let s0 = Command::new(exe)
        .args(["a", "archive_lvl0.zip", test_dir, "-mx=0"])
        .status()
        .unwrap();
    assert!(s0.success());

    // 2. Create with level 1 (Fast Deflate)
    let s1 = Command::new(exe)
        .args(["a", "archive_lvl1.zip", test_dir, "-mx=1"])
        .status()
        .unwrap();
    assert!(s1.success());

    // 3. Create with level 9 (Ultra Deflate)
    let s9 = Command::new(exe)
        .args(["a", "archive_lvl9.zip", test_dir, "-mx=9"])
        .status()
        .unwrap();
    assert!(s9.success());

    let size_0 = fs::metadata("archive_lvl0.zip").unwrap().len();
    let size_1 = fs::metadata("archive_lvl1.zip").unwrap().len();
    let size_9 = fs::metadata("archive_lvl9.zip").unwrap().len();

    // Deflate level 9 must be dramatically smaller than Stored level 0, and
    // smaller than level 1: the latter proves the level reaches the encoder.
    assert!(
        size_9 < size_0,
        "level 9 ({}) should be smaller than level 0 ({})",
        size_9,
        size_0
    );
    assert!(
        size_9 < size_1,
        "level 9 ({}) should be smaller than level 1 ({})",
        size_9,
        size_1
    );

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
    let _ = fs::remove_file("archive_lvl1.zip");
    let _ = fs::remove_file("archive_lvl9.zip");
}

#[test]
fn test_7z_compression_options() {
    let test_dir = "test_data_7z_opts";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(
        format!("{}/f1.txt", test_dir),
        "File 1 content repeated ".repeat(100),
    )
    .unwrap();
    fs::write(
        format!("{}/sub/f2.txt", test_dir),
        "File 2 content repeated ".repeat(100),
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with -mx=1, -ms=off, -mmt=1
    let s1 = Command::new(exe)
        .args([
            "a",
            "archive_fast_nonsolid.7z",
            test_dir,
            "-mx=1",
            "-ms=off",
            "-mmt=1",
        ])
        .status()
        .unwrap();
    assert!(s1.success());

    // 2. Create with -mx=9, -ms=on, -md=16m, -mmt=on
    let s9 = Command::new(exe)
        .args([
            "a",
            "archive_ultra_solid.7z",
            test_dir,
            "-mx=9",
            "-ms=on",
            "-md=16m",
            "-mmt=on",
        ])
        .status()
        .unwrap();
    assert!(s9.success());

    // 3. Same level, larger dictionary: properties must change.
    let s_dict = Command::new(exe)
        .args([
            "a",
            "archive_dict64.7z",
            test_dir,
            "-mx=5",
            "-ms=on",
            "-md=64m",
            "-mmt=1",
        ])
        .status()
        .unwrap();
    assert!(s_dict.success());

    // 4. Level 0 must store entries, matching 7-Zip's `-mx=0`.
    let s_store = Command::new(exe)
        .args(["a", "archive_store.7z", test_dir, "-mx=0"])
        .status()
        .unwrap();
    assert!(s_store.success());
    assert!(
        has_coder("archive_store.7z", sevenz_rust2::EncoderMethod::ID_COPY),
        "-mx=0 should use the COPY coder"
    );
    assert!(
        !has_coder("archive_store.7z", sevenz_rust2::EncoderMethod::ID_LZMA2),
        "-mx=0 should not use LZMA2"
    );

    // Solid mode must control block layout: off = one block per file,
    // on = one solid block for this small fixture.
    assert_eq!(
        Archive::open("archive_fast_nonsolid.7z")
            .unwrap()
            .blocks
            .len(),
        2,
        "-ms=off should create one block per file"
    );
    assert_eq!(
        Archive::open("archive_ultra_solid.7z")
            .unwrap()
            .blocks
            .len(),
        1,
        "-ms=on should create one solid block"
    );

    // The LZMA2 coder properties encode the dictionary size, which is derived
    // from the preset and overridden by -md. Different properties prove that
    // -mx and -md reach the encoder.
    let fast_props = lzma2_properties("archive_fast_nonsolid.7z");
    let ultra_props = lzma2_properties("archive_ultra_solid.7z");
    let dict64_props = lzma2_properties("archive_dict64.7z");
    assert_ne!(
        fast_props, ultra_props,
        "-mx=1 and -mx=9 should select different LZMA2 dictionaries"
    );
    assert_ne!(
        ultra_props, dict64_props,
        "-md=16m and -md=64m should select different LZMA2 dictionaries"
    );

    // Test both archives
    assert!(
        Command::new(exe)
            .args(["t", "archive_fast_nonsolid.7z"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(exe)
            .args(["t", "archive_ultra_solid.7z"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(exe)
            .args(["t", "archive_store.7z"])
            .status()
            .unwrap()
            .success()
    );

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
    let _ = fs::remove_file("archive_dict64.7z");
    let _ = fs::remove_file("archive_store.7z");
}

#[test]
fn test_tar_roundtrip_formats() {
    let test_dir = "test_data_tar_roundtrip";
    fs::create_dir_all(format!("{}/sub", test_dir)).unwrap();
    fs::write(format!("{}/small.txt", test_dir), "tar small file").unwrap();
    fs::write(format!("{}/sub/empty.bin", test_dir), b"").unwrap();
    let payload = vec![0x5A; 256 * 1024];
    fs::write(format!("{}/sub/payload.bin", test_dir), &payload).unwrap();
    let long_name = format!("long_{}.txt", "segment_".repeat(20));
    fs::write(
        format!("{}/sub/{}", test_dir, long_name),
        "long path payload",
    )
    .unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    for ext in ["tar", "tar.gz", "tar.bz2"] {
        let archive = format!("archive_roundtrip.{}", ext);
        let created = Command::new(exe)
            .args(["a", &archive, test_dir, "-mx=5"])
            .status()
            .unwrap();
        assert!(created.success(), "failed to create {}", archive);

        let tested = Command::new(exe).args(["t", &archive]).status().unwrap();
        assert!(tested.success(), "failed to test {}", archive);

        let out_dir = format!("out_tar_{}", ext.replace('.', "_"));
        let extracted = Command::new(exe)
            .args(["x", &archive, "-o", &out_dir])
            .status()
            .unwrap();
        assert!(extracted.success(), "failed to extract {}", archive);

        let base = PathBuf::from(&out_dir).join(test_dir);
        assert_eq!(
            fs::read_to_string(base.join("small.txt")).unwrap(),
            "tar small file"
        );
        assert_eq!(
            fs::read(base.join("sub/empty.bin")).unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(fs::read(base.join("sub/payload.bin")).unwrap(), payload);
        assert_eq!(
            fs::read_to_string(base.join("sub").join(&long_name)).unwrap(),
            "long path payload"
        );

        let _ = fs::remove_dir_all(&out_dir);
        let _ = fs::remove_file(&archive);
    }

    let _ = fs::remove_dir_all(test_dir);
}

/// Build an in-memory TAR with a few entries whose combined data spans
/// several 512-byte records, so a split lands mid-entry.
fn sample_tar_bytes(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, &data[..]).unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn test_tar_reads_multi_member_streams() {
    use std::io::Write;

    // pigz/pbzip2-style output: the stream is split into independent gzip
    // members / bzip2 streams and concatenated. Decoders must read past the
    // first member instead of stopping (or reporting a truncated archive).
    let entries: Vec<(&str, Vec<u8>)> = vec![
        ("multi/a.txt", b"first entry".to_vec()),
        ("multi/b.bin", (0..200_000u32).map(|i| (i % 251) as u8).collect()),
        ("multi/c.txt", b"last entry".to_vec()),
    ];
    let tar_bytes = sample_tar_bytes(&entries);
    let parts: Vec<&[u8]> = tar_bytes.chunks(tar_bytes.len() / 3 + 1).collect();
    assert!(parts.len() >= 3);

    let mut gz = Vec::new();
    let mut bz = Vec::new();
    for part in &parts {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
        enc.write_all(part).unwrap();
        gz.extend(enc.finish().unwrap());
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(9));
        enc.write_all(part).unwrap();
        bz.extend(enc.finish().unwrap());
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");
    for (ext, data) in [("tar.gz", gz), ("tar.bz2", bz)] {
        let archive = format!("archive_multi_member.{}", ext);
        fs::write(&archive, data).unwrap();

        let tested = Command::new(exe).args(["t", &archive]).status().unwrap();
        assert!(tested.success(), "failed to test {}", archive);

        let out_dir = format!("out_multi_member_{}", ext.replace('.', "_"));
        let extracted = Command::new(exe)
            .args(["x", &archive, "-o", &out_dir])
            .status()
            .unwrap();
        assert!(extracted.success(), "failed to extract {}", archive);
        for (name, data) in &entries {
            assert_eq!(
                &fs::read(PathBuf::from(&out_dir).join(name)).unwrap(),
                data,
                "{} mismatch in {}",
                name,
                archive
            );
        }

        let _ = fs::remove_dir_all(&out_dir);
        let _ = fs::remove_file(&archive);
    }
}

/// Decode a compressed TAR with the standard decoder crates (no OpenArchive
/// code involved) and return (path, contents) for each regular file.
fn decode_tar_with_std_crates(path: &str) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;

    let file = fs::File::open(path).unwrap();
    let reader: Box<dyn Read> = if path.ends_with(".gz") {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(bzip2::read::MultiBzDecoder::new(file))
    };
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.header().entry_type().is_file() {
            let name = entry.path().unwrap().to_string_lossy().replace('\\', "/");
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            out.push((name, data));
        }
    }
    out.sort();
    out
}

/// Count gzip members by decoding them one at a time.
fn gzip_member_count(data: &[u8]) -> usize {
    use std::io::Read;

    let mut rest = data;
    let mut count = 0;
    while !rest.is_empty() {
        let mut decoder = flate2::bufread::GzDecoder::new(rest);
        std::io::copy(&mut decoder, &mut std::io::sink()).unwrap();
        let mut remaining = Vec::new();
        decoder.into_inner().read_to_end(&mut remaining).unwrap();
        let consumed = rest.len() - remaining.len();
        assert!(consumed > 0);
        rest = &rest[consumed..];
        count += 1;
    }
    count
}

#[test]
fn test_tar_parallel_compression_roundtrip() {
    let test_dir = "test_data_tar_parallel";
    fs::create_dir_all(format!("{}/nested", test_dir)).unwrap();

    // ~6 MB of mixed content: compressible text plus pseudo-random noise,
    // enough for several gzip chunks and bzip2 streams.
    let mut text = String::new();
    for i in 0..60_000 {
        text.push_str(&format!("line {i}: parallel tar payload {}\n", (i * 7919) % 10_007));
    }
    let mut noise = Vec::with_capacity(3 * 1024 * 1024);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..noise.capacity() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        noise.push(x as u8);
    }
    fs::write(format!("{}/text.txt", test_dir), &text).unwrap();
    fs::write(format!("{}/nested/noise.bin", test_dir), &noise).unwrap();
    fs::write(format!("{}/nested/empty.txt", test_dir), b"").unwrap();
    for i in 0..50 {
        fs::write(format!("{}/nested/small_{i}.txt", test_dir), format!("small {i}")).unwrap();
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");
    for ext in ["tar.gz", "tar.bz2"] {
        let mut decoded_by_threads = Vec::new();
        for threads in ["1", "4"] {
            let archive = format!("archive_parallel_mmt{}.{}", threads, ext);
            let created = Command::new(exe)
                .args(["a", &archive, test_dir, "-mx=6", &format!("-mmt={}", threads)])
                .status()
                .unwrap();
            assert!(created.success(), "failed to create {}", archive);

            let bytes = fs::read(&archive).unwrap();
            if ext == "tar.gz" {
                let members = gzip_member_count(&bytes);
                if threads == "1" {
                    assert_eq!(members, 1, "-mmt=1 must keep a single gzip member");
                } else {
                    assert!(members > 1, "-mmt=4 should produce several members");
                }
            } else {
                assert_eq!(&bytes[..4], b"BZh6");
            }

            // Standard decoder crates read every member.
            let decoded = decode_tar_with_std_crates(&archive);
            let lookup = |name: &str| {
                decoded
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, d)| d.clone())
                    .unwrap_or_else(|| panic!("{} missing from {}", name, archive))
            };
            assert_eq!(lookup(&format!("{}/text.txt", test_dir)), text.as_bytes());
            assert_eq!(lookup(&format!("{}/nested/noise.bin", test_dir)), noise);
            assert!(lookup(&format!("{}/nested/empty.txt", test_dir)).is_empty());
            assert_eq!(decoded.len(), 53);
            decoded_by_threads.push(decoded);

            // And OpenArchive's own extraction.
            let out_dir = format!("out_tar_parallel_mmt{}_{}", threads, ext.replace('.', "_"));
            let extracted = Command::new(exe)
                .args(["x", &archive, "-o", &out_dir])
                .status()
                .unwrap();
            assert!(extracted.success(), "failed to extract {}", archive);
            let base = PathBuf::from(&out_dir).join(test_dir);
            assert_eq!(fs::read(base.join("nested/noise.bin")).unwrap(), noise);
            assert_eq!(fs::read_to_string(base.join("text.txt")).unwrap(), text);

            let _ = fs::remove_dir_all(&out_dir);
            let _ = fs::remove_file(&archive);
        }
        assert_eq!(decoded_by_threads[0], decoded_by_threads[1]);
    }

    let _ = fs::remove_dir_all(test_dir);
}

#[test]
fn test_tar_large_entries_use_sequential_path() {
    let test_dir = "test_data_tar_large_entries";
    fs::create_dir_all(test_dir).unwrap();
    let first = vec![0x11; 2 * 1024 * 1024];
    let second = vec![0x22; 2 * 1024 * 1024];
    fs::write(format!("{}/first.bin", test_dir), &first).unwrap();
    fs::write(format!("{}/second.bin", test_dir), &second).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    let created = Command::new(exe)
        .args(["a", "archive_large_entries.tar", test_dir])
        .status()
        .unwrap();
    assert!(created.success());

    let extracted = Command::new(exe)
        .args([
            "x",
            "archive_large_entries.tar",
            "-o",
            "out_tar_large_entries",
            "-mmt=4",
        ])
        .status()
        .unwrap();
    assert!(extracted.success());

    let base = PathBuf::from("out_tar_large_entries").join(test_dir);
    assert_eq!(fs::read(base.join("first.bin")).unwrap(), first);
    assert_eq!(fs::read(base.join("second.bin")).unwrap(), second);

    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_tar_large_entries");
    let _ = fs::remove_file("archive_large_entries.tar");
}

#[test]
fn test_zip_parallel_roundtrip() {
    let test_dir = "test_data_zip_parallel";
    fs::create_dir_all(format!("{}/sub1", test_dir)).unwrap();
    fs::create_dir_all(format!("{}/sub2/nested", test_dir)).unwrap();

    let mut expected_files = std::collections::HashMap::new();
    for i in 0..30 {
        let path = if i < 10 {
            format!("{}/file_{}.txt", test_dir, i)
        } else if i < 20 {
            format!("{}/sub1/file_{}.txt", test_dir, i)
        } else {
            format!("{}/sub2/nested/file_{}.txt", test_dir, i)
        };
        let content = format!(
            "Parallel test content {} - line 1\nLine 2 payload {}\n",
            i,
            "X".repeat(100 * 1024)
        );
        fs::write(&path, &content).unwrap();
        expected_files.insert(path, content);
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with 4 threads. The batch-size override yields ~15 batches,
    // each large enough to complete out of order, so the bounded reorder
    // window is exercised on a small fixture.
    let s_create = Command::new(exe)
        .args(["a", "archive_parallel.zip", test_dir, "-mmt=4", "-mx=6"])
        .env("OPENARCHIVE_ZIP_BATCH_BYTES", "262144")
        .status()
        .unwrap();
    assert!(s_create.success());

    // 2. Test archive integrity
    let s_test = Command::new(exe)
        .args(["t", "archive_parallel.zip"])
        .status()
        .unwrap();
    assert!(s_test.success());

    // 3. Extract with 4 threads (parallel extraction)
    let s_extract = Command::new(exe)
        .args([
            "x",
            "archive_parallel.zip",
            "-o",
            "out_zip_parallel",
            "-mmt=4",
        ])
        .status()
        .unwrap();
    assert!(s_extract.success());

    // 4. Verify all files
    for (orig_path, content) in &expected_files {
        let extracted_path = format!("out_zip_parallel/{}", orig_path);
        let extracted_content = fs::read_to_string(&extracted_path)
            .unwrap_or_else(|_| panic!("Failed to read extracted file: {}", extracted_path));
        assert_eq!(
            &extracted_content, content,
            "Content mismatch in {}",
            extracted_path
        );
    }

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_zip_parallel");
    let _ = fs::remove_file("archive_parallel.zip");
}

#[test]
fn test_zip_large_file_spill_roundtrip() {
    let test_dir = "test_data_zip_bigfile";
    fs::create_dir_all(test_dir).unwrap();

    // All of these exceed the configured batch limit, so each is prepared in
    // a temporary sub-archive instead of forcing a sequential archive run.
    let mut expected = std::collections::HashMap::new();
    for index in 0..4u8 {
        let name = format!("big_{index}.bin");
        let payload = vec![index.wrapping_mul(37).wrapping_add(11); 300 * 1024];
        fs::write(format!("{}/{}", test_dir, name), &payload).unwrap();
        expected.insert(name, payload);
    }
    fs::write(format!("{}/small.txt", test_dir), "small file").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    let created = Command::new(exe)
        .args(["a", "archive_bigfile.zip", test_dir, "-mmt=4", "-mx=6"])
        .env("OPENARCHIVE_ZIP_BATCH_BYTES", "262144")
        .status()
        .unwrap();
    assert!(created.success());

    let tested = Command::new(exe)
        .args(["t", "archive_bigfile.zip"])
        .status()
        .unwrap();
    assert!(tested.success());

    let extracted = Command::new(exe)
        .args([
            "x",
            "archive_bigfile.zip",
            "-o",
            "out_zip_bigfile",
            "-mmt=4",
        ])
        .status()
        .unwrap();
    assert!(extracted.success());

    for (name, payload) in &expected {
        let path = format!("out_zip_bigfile/test_data_zip_bigfile/{}", name);
        assert_eq!(&fs::read(&path).unwrap(), payload, "mismatch in {}", path);
    }
    assert_eq!(
        fs::read_to_string("out_zip_bigfile/test_data_zip_bigfile/small.txt").unwrap(),
        "small file"
    );

    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_zip_bigfile");
    let _ = fs::remove_file("archive_bigfile.zip");
}

#[test]
fn test_tar_compression_levels() {
    let test_dir = "test_data_tar_levels";
    fs::create_dir_all(test_dir).unwrap();

    let mut sample = String::with_capacity(1024 * 1024);
    for i in 0..20_000 {
        sample.push_str(&format!(
            "tar line {i}: compression level payload {}\n",
            (i * 104_729) % 99_991
        ));
    }
    fs::write(format!("{}/large.txt", test_dir), &sample).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    for ext in ["tar.gz", "tar.bz2"] {
        for level in [1, 9] {
            let archive = format!("archive_lvl{}.{}", level, ext);
            let status = Command::new(exe)
                .args(["a", &archive, test_dir, &format!("-mx={}", level)])
                .status()
                .unwrap();
            assert!(status.success(), "failed to create {}", archive);
        }

        let size_1 = fs::metadata(format!("archive_lvl1.{}", ext)).unwrap().len();
        let size_9 = fs::metadata(format!("archive_lvl9.{}", ext)).unwrap().len();

        if ext == "tar.gz" {
            // Deflate level differences show up as size differences.
            assert!(
                size_9 < size_1,
                "{}: level 9 ({}) should be smaller than level 1 ({})",
                ext,
                size_9,
                size_1
            );
        } else {
            // Bzip2 encodes its block size (1-9) in the "BZhN" header. Level 9
            // is not always smaller on small inputs, so check the header
            // directly to prove the level reached the encoder.
            let header1 = fs::read(format!("archive_lvl1.{}", ext)).unwrap();
            let header9 = fs::read(format!("archive_lvl9.{}", ext)).unwrap();
            assert_eq!(&header1[..4], b"BZh1");
            assert_eq!(&header9[..4], b"BZh9");
        }

        let tested = Command::new(exe)
            .args(["t", &format!("archive_lvl9.{}", ext)])
            .status()
            .unwrap();
        assert!(tested.success(), "failed to test level 9 {}", ext);
    }

    let _ = fs::remove_dir_all(test_dir);
    for ext in ["tar.gz", "tar.bz2"] {
        let _ = fs::remove_file(format!("archive_lvl1.{}", ext));
        let _ = fs::remove_file(format!("archive_lvl9.{}", ext));
    }
}

#[test]
fn test_7z_single_block_mt_roundtrip() {
    let test_dir = "test_data_7z_mt";
    fs::create_dir_all(test_dir).unwrap();

    // Above the 8 MB MT threshold, so a single-file archive exercises the
    // multi-threaded LZMA2 streaming path rather than the block pipeline.
    let sample = "Multi-threaded LZMA2 single block test.\n".repeat(300_000);
    let src_path = format!("{}/big.txt", test_dir);
    fs::write(&src_path, &sample).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    let s_create = Command::new(exe)
        .args(["a", "archive_mt.7z", &src_path, "-mmt=4", "-mx=5"])
        .status()
        .unwrap();
    assert!(s_create.success());

    let archive = Archive::open("archive_mt.7z").unwrap();
    assert_eq!(
        archive.blocks.len(),
        1,
        "a single file should produce one solid block"
    );

    let s_test = Command::new(exe)
        .args(["t", "archive_mt.7z"])
        .status()
        .unwrap();
    assert!(s_test.success());

    let s_extract = Command::new(exe)
        .args(["x", "archive_mt.7z", "-o", "out_7z_mt", "-mmt=4"])
        .status()
        .unwrap();
    assert!(s_extract.success());

    let extracted = fs::read_to_string("out_7z_mt/big.txt").unwrap();
    assert_eq!(extracted, sample);

    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_7z_mt");
    let _ = fs::remove_file("archive_mt.7z");
}

#[test]
fn test_7z_parallel_roundtrip() {
    let test_dir = "test_data_7z_parallel";
    fs::create_dir_all(format!("{}/sub1", test_dir)).unwrap();
    fs::create_dir_all(format!("{}/sub2/nested", test_dir)).unwrap();

    let mut expected_files = std::collections::HashMap::new();
    for i in 0..25 {
        let path = if i < 8 {
            format!("{}/file_{}.txt", test_dir, i)
        } else if i < 16 {
            format!("{}/sub1/file_{}.txt", test_dir, i)
        } else {
            format!("{}/sub2/nested/file_{}.txt", test_dir, i)
        };
        // Large enough that the test hook below splits this fixture into
        // several solid blocks, which is the path under test.
        let content = format!(
            "7z Parallel test content {} - line 1\nLine 2 payload {}\n",
            i,
            "Y".repeat(600 * 1024)
        );
        fs::write(&path, &content).unwrap();
        expected_files.insert(path, content);
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // 1. Create with -mmt=4, -ms=on. The block-size override makes a small
    // fixture produce several independent blocks so the parallel pipeline is
    // actually exercised.
    let s_create = Command::new(exe)
        .args([
            "a",
            "archive_parallel.7z",
            test_dir,
            "-mmt=4",
            "-ms=on",
            "-mx=5",
        ])
        .env("OPENARCHIVE_7Z_BLOCK_SIZE", "1048576")
        .status()
        .unwrap();
    assert!(s_create.success());

    let archive = Archive::open("archive_parallel.7z").unwrap();
    assert!(
        archive.blocks.len() >= 2,
        "expected a multi-block archive, got {} block(s)",
        archive.blocks.len()
    );

    // 2. Test integrity
    let s_test = Command::new(exe)
        .args(["t", "archive_parallel.7z"])
        .status()
        .unwrap();
    assert!(s_test.success());

    // 3. Extract with -mmt=4 (parallel block decode, serialized writer)
    let s_extract = Command::new(exe)
        .args([
            "x",
            "archive_parallel.7z",
            "-o",
            "out_7z_parallel",
            "-mmt=4",
        ])
        .status()
        .unwrap();
    assert!(s_extract.success());

    // 4. Verify all files
    for (orig_path, content) in &expected_files {
        let extracted_path = format!("out_7z_parallel/{}", orig_path);
        let extracted_content = fs::read_to_string(&extracted_path)
            .unwrap_or_else(|_| panic!("Failed to read extracted file: {}", extracted_path));
        assert_eq!(
            &extracted_content, content,
            "Content mismatch in {}",
            extracted_path
        );
    }

    // Cleanup
    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_7z_parallel");
    let _ = fs::remove_file("archive_parallel.7z");
}
