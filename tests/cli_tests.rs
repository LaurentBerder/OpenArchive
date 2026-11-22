use std::fs;
use std::path::{Path, PathBuf};
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

    for ext in ["tar", "tar.gz", "tar.bz2", "tar.xz"] {
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

/// TAR with a raw entry name: `tar::Builder::append_data` refuses `..`, so
/// the header name bytes are written directly.
fn tar_with_raw_names(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        let name_field = &mut header.as_old_mut().name;
        assert!(name.len() < name_field.len());
        name_field[..name.len()].copy_from_slice(name.as_bytes());
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn test_extraction_rejects_path_traversal() {
    use std::io::Write;

    let sandbox = PathBuf::from("slip_sandbox");
    let _ = fs::remove_dir_all(&sandbox);
    fs::create_dir_all(&sandbox).unwrap();
    let out_dir = sandbox.join("out");
    // An absolute name that, if honoured, lands inside the sandbox rather
    // than somewhere unexpected on the machine.
    let absolute = fs::canonicalize(&sandbox)
        .unwrap()
        .join("abs_escape.txt")
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();

    let exe = env!("CARGO_BIN_EXE_openarchive");
    // Extraction must refuse the archive and nothing may land outside the
    // output directory. With `up_front`, the format sees every name before
    // writing (zip, 7z, indexed plain TAR), so not even the safe entry that
    // comes first may be written; streamed TAR stops at the bad entry.
    let check = |command: &str, archive: &PathBuf, extra_args: &[&str], label: &str, up_front: bool| {
        let _ = fs::remove_dir_all(&out_dir);
        let mut args = vec![
            command.to_string(),
            archive.to_string_lossy().to_string(),
            "-o".to_string(),
            out_dir.to_string_lossy().to_string(),
        ];
        args.extend(extra_args.iter().map(|s| s.to_string()));
        let output = Command::new(exe).args(&args).output().unwrap();
        assert!(!output.status.success(), "{label}: extraction should fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unsafe archive entry"),
            "{label}: unexpected error: {stderr}"
        );
        for escaped in ["escape.txt", "escape2.txt", "abs_escape.txt"] {
            assert!(
                !sandbox.join(escaped).exists(),
                "{label}: {escaped} escaped the output directory"
            );
        }
        if up_front {
            // `x` would write safe/ok.txt, flat `e` would write ok.txt.
            assert!(
                !out_dir.join("safe").join("ok.txt").exists() && !out_dir.join("ok.txt").exists(),
                "{label}: nothing should be written before refusing"
            );
        }
    };

    // TAR / TAR.GZ: plain TAR takes the parallel indexed path with threads
    // and the sequential path with -mmt=1; TAR.GZ is always sequential.
    let tar_bytes = tar_with_raw_names(&[
        ("safe/ok.txt", b"safe"),
        ("../escape.txt", b"evil"),
        ("safe/../../escape2.txt", b"evil"),
    ]);
    let tar_path = sandbox.join("slip.tar");
    fs::write(&tar_path, &tar_bytes).unwrap();
    check("x", &tar_path, &["-mmt=4"], "tar parallel", true);
    check("x", &tar_path, &["-mmt=1"], "tar sequential", false);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    gz.write_all(&tar_bytes).unwrap();
    let tgz_path = sandbox.join("slip.tar.gz");
    fs::write(&tgz_path, gz.finish().unwrap()).unwrap();
    check("x", &tgz_path, &[], "tar.gz", false);

    // ZIP, including an absolute name.
    let zip_path = sandbox.join("slip.zip");
    {
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in [
            ("safe/ok.txt", "safe"),
            ("../escape.txt", "evil"),
            ("safe/../../escape2.txt", "evil"),
            (absolute.as_str(), "evil"),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    check("x", &zip_path, &["-mmt=4"], "zip parallel", true);
    check("e", &zip_path, &["-mmt=4"], "zip flat", true);
    check("x", &zip_path, &["-mmt=1"], "zip sequential", true);

    // 7z, sequential and pipelined extraction.
    let sz_path = sandbox.join("slip.7z");
    {
        let mut writer = sevenz_rust2::ArchiveWriter::create(&sz_path).unwrap();
        for (name, data) in [
            ("safe/ok.txt", "safe"),
            ("../escape.txt", "evil"),
            ("safe/../../escape2.txt", "evil"),
            (absolute.as_str(), "evil"),
        ] {
            writer
                .push_archive_entry(
                    sevenz_rust2::ArchiveEntry::new_file(name),
                    Some(data.as_bytes()),
                )
                .unwrap();
        }
        writer.finish().unwrap();
    }
    check("x", &sz_path, &["-mmt=4"], "7z pipelined", true);
    check("e", &sz_path, &["-mmt=4"], "7z flat", true);
    check("x", &sz_path, &["-mmt=1"], "7z sequential", true);

    let _ = fs::remove_dir_all(&sandbox);
}

#[test]
fn test_tar_symlink_entries_are_not_followed() {
    // `d -> ../..` followed by `d/x`: if extraction created the link, `d/x`
    // would pass the name check yet land two levels above the output dir.
    let sandbox = PathBuf::from("symlink_sandbox");
    let _ = fs::remove_dir_all(&sandbox);
    let out_dir = sandbox.join("deep").join("out");
    fs::create_dir_all(&out_dir).unwrap();

    let mut builder = tar::Builder::new(Vec::new());
    for (name, target) in [("d", "../.."), ("h", "../../hard_target")] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(if name == "d" {
            tar::EntryType::Symlink
        } else {
            tar::EntryType::Link
        });
        header.set_size(0);
        header.set_mode(0o777);
        // `set_link_name` refuses `..`; write the raw field instead.
        header.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        builder.append(&header, &[][..]).unwrap();
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(4);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "d/x", &b"evil"[..]).unwrap();
    let archive = sandbox.join("links.tar");
    fs::write(&archive, builder.into_inner().unwrap()).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");
    for threads in ["-mmt=1", "-mmt=4"] {
        // Writing `d/x` under a regular file `d` may fail; what matters is
        // that nothing is written through a link.
        let _ = Command::new(exe)
            .args(["x", archive.to_str().unwrap(), "-o", out_dir.to_str().unwrap(), threads])
            .output()
            .unwrap();
        assert!(!sandbox.join("x").exists(), "{threads}: d/x escaped via symlink");
        assert!(!sandbox.join("deep").join("x").exists(), "{threads}: escaped");
        for link in ["d", "h"] {
            if let Ok(meta) = fs::symlink_metadata(out_dir.join(link)) {
                assert!(!meta.file_type().is_symlink(), "{threads}: {link} became a symlink");
            }
        }
    }

    let _ = fs::remove_dir_all(&sandbox);
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
fn test_zip_mmap_large_file_roundtrip() {
    let test_dir = "test_data_zip_mmap";
    fs::create_dir_all(test_dir).unwrap();
    let payload: Vec<u8> = (0..2 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(format!("{}/large.bin", test_dir), &payload).unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");

    // Lower the threshold so this small fixture goes through the memory-mapped
    // windowed-read path.
    let created = Command::new(exe)
        .args(["a", "archive_mmap.zip", test_dir, "-mx=0", "-mmt=1"])
        .env("OPENARCHIVE_MMAP_THRESHOLD_MB", "1")
        .status()
        .unwrap();
    assert!(created.success());

    let tested = Command::new(exe)
        .args(["t", "archive_mmap.zip"])
        .status()
        .unwrap();
    assert!(tested.success());

    let extracted = Command::new(exe)
        .args(["x", "archive_mmap.zip", "-o", "out_zip_mmap"])
        .status()
        .unwrap();
    assert!(extracted.success());

    assert_eq!(
        fs::read("out_zip_mmap/test_data_zip_mmap/large.bin").unwrap(),
        payload
    );

    let _ = fs::remove_dir_all(test_dir);
    let _ = fs::remove_dir_all("out_zip_mmap");
    let _ = fs::remove_file("archive_mmap.zip");
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

// ─── Path traversal: rooted names ───────────────────────────────────────────

#[test]
fn test_rooted_entry_extracts_inside_output_dir() {
    // A leading separator is dropped (as 7-Zip and GNU tar do), so the entry
    // lands inside the output directory instead of being refused.
    let dir = "test_slip_rooted";
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    let archive = format!("{dir}/rooted.tar");
    fs::write(&archive, tar_with_raw_names(&[("/rooted.txt", b"rooted")])).unwrap();

    let out = format!("{dir}/out");
    let exe = env!("CARGO_BIN_EXE_openarchive");
    let status = Command::new(exe)
        .args(["x", &archive, "-o", &out, "-mmt=1"])
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(format!("{out}/rooted.txt")).unwrap(), b"rooted");

    let _ = fs::remove_dir_all(dir);
}

// ─── Entry filter regression tests ──────────────────────────────────────────

/// Relative paths of all files under `root`, with `/` separators, sorted.
fn files_under(root: &str) -> Vec<String> {
    let mut files: Vec<String> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn test_filter_selects_exact_entries_only() {
    let dir = "test_filter";
    let _ = fs::remove_dir_all(dir);
    let src = format!("{dir}/flt");
    // Every file contains its own relative path, so collisions are visible.
    for rel in [
        "a.txt",
        "data.txt",
        "a.txt.bak",
        "sub/a.txt",
        "docs/x.txt",
        "docs/deep/z.txt",
        "docs2/y.txt",
    ] {
        let path = PathBuf::from(&src).join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, rel).unwrap();
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");
    for ext in ["zip", "7z", "tar", "tar.gz"] {
        let archive = format!("{dir}/filter.{ext}");
        let status = Command::new(exe)
            .current_dir(dir)
            .args(["a", &format!("filter.{ext}"), "flt"])
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "creating {archive}");

        for threads in ["-mmt=1", "-mmt=4"] {
            // Full-path extraction: one file and one directory.
            let out = format!("{dir}/out_{ext}_{threads}");
            let _ = fs::remove_dir_all(&out);
            let status = Command::new(exe)
                .args(["x", &archive, "flt/a.txt", "flt/docs", "-o", &out, threads])
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "extracting {archive} {threads}");
            assert_eq!(
                files_under(&out),
                ["flt/a.txt", "flt/docs/deep/z.txt", "flt/docs/x.txt"],
                "{archive} {threads}"
            );

            // Flat extraction of a single file must not pull in
            // `sub/a.txt`, `data.txt` or `a.txt.bak`.
            let flat = format!("{dir}/flat_{ext}_{threads}");
            let _ = fs::remove_dir_all(&flat);
            let status = Command::new(exe)
                .args(["e", &archive, "flt/a.txt", "-o", &flat, threads])
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "flat extracting {archive} {threads}");
            assert_eq!(files_under(&flat), ["a.txt"], "{archive} {threads}");
            assert_eq!(fs::read_to_string(format!("{flat}/a.txt")).unwrap(), "a.txt");
        }
    }

    let _ = fs::remove_dir_all(dir);
}

// ─── Zip encryption ─────────────────────────────────────────────────────────

/// Plaintext that must never appear in an encrypted archive's bytes.
const ZIP_SECRET_MARKER: &[u8] = b"OPENARCHIVE-PLAINTEXT-MARKER-7f3a";

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn test_zip_password_encrypts_entries() {
    let dir = "test_zip_password";
    let _ = fs::remove_dir_all(dir);
    let src = format!("{dir}/src");
    fs::create_dir_all(format!("{src}/sub")).unwrap();
    // Several files so the batch paths have more than one batch to merge.
    let mut files = Vec::new();
    for i in 0..6 {
        let mut payload = ZIP_SECRET_MARKER.to_vec();
        payload.extend((0..64 * 1024).map(|j| ((i * 31 + j) % 251) as u8));
        let rel = if i % 2 == 0 { format!("f{i}.bin") } else { format!("sub/f{i}.bin") };
        fs::write(format!("{src}/{rel}"), &payload).unwrap();
        files.push((rel, payload));
    }

    let exe = env!("CARGO_BIN_EXE_openarchive");
    // (label, extra args, batch bytes): sequential; parallel in-memory batches;
    // parallel batches spilled to temp files (batch limit below one file).
    let cases: [(&str, &[&str], Option<&str>); 3] = [
        ("sequential", &["-mmt=1"], None),
        ("parallel", &["-mmt=4"], Some("150000")),
        ("spill", &["-mmt=4"], Some("1000")),
    ];

    for (label, extra, batch_bytes) in cases {
        for level in ["-mx=0", "-mx=5"] {
            let archive = format!("{dir}/enc_{label}{level}.zip");
            let mut create = Command::new(exe);
            create
                .current_dir(dir)
                .args(["a", &archive[dir.len() + 1..], "src", "-psecret", level])
                .args(extra);
            if let Some(bytes) = batch_bytes {
                create.env("OPENARCHIVE_ZIP_BATCH_BYTES", bytes);
            }
            assert!(create.output().unwrap().status.success(), "{label}{level}: create");

            // No plaintext anywhere in the file, even when stored (-mx=0).
            let bytes = fs::read(&archive).unwrap();
            assert!(
                !contains_bytes(&bytes, ZIP_SECRET_MARKER),
                "{label}{level}: plaintext found in archive"
            );

            // Every file entry is encrypted.
            let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
            let mut file_entries = 0;
            for i in 0..zip.len() {
                let entry = zip.by_index_raw(i).unwrap();
                if !entry.is_dir() {
                    assert!(entry.encrypted(), "{label}{level}: {} not encrypted", entry.name());
                    file_entries += 1;
                }
            }
            assert_eq!(file_entries, files.len(), "{label}{level}: entry count");

            // Missing and wrong passwords fail.
            let out = format!("{dir}/out_{label}{level}");
            for password in [None, Some("-pwrong")] {
                let _ = fs::remove_dir_all(&out);
                let mut cmd = Command::new(exe);
                cmd.args(["x", &archive, "-o", &out]);
                if let Some(p) = password {
                    cmd.arg(p);
                }
                assert!(
                    !cmd.output().unwrap().status.success(),
                    "{label}{level}: extraction with {password:?} should fail"
                );
            }
            assert!(
                !Command::new(exe).args(["t", &archive]).output().unwrap().status.success(),
                "{label}{level}: test without password should fail"
            );

            // The right password round-trips every file.
            let _ = fs::remove_dir_all(&out);
            let status = Command::new(exe)
                .args(["x", &archive, "-o", &out, "-psecret"])
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "{label}{level}: extract with password");
            for (rel, payload) in &files {
                assert_eq!(
                    &fs::read(format!("{out}/src/{rel}")).unwrap(),
                    payload,
                    "{label}{level}: {rel}"
                );
            }
            let tested = Command::new(exe).args(["t", &archive, "-psecret"]).output().unwrap();
            assert!(tested.status.success(), "{label}{level}: test with password");
        }
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn test_zip_empty_password_means_unencrypted() {
    let dir = "test_zip_empty_password";
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
    fs::write(format!("{dir}/a.txt"), "plain").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");
    let archive = format!("{dir}/plain.zip");
    let status = Command::new(exe)
        // `-p ""` yields an empty password (a bare trailing `-p=` yields none).
        .args(["a", &archive, &format!("{dir}/a.txt"), "-p", ""])
        .output()
        .unwrap()
        .status;
    assert!(status.success());
    let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
    assert!(!zip.by_index_raw(0).unwrap().encrypted());

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn test_tar_skipped_links_are_reported_on_stderr() {
    let dir = PathBuf::from("test_tar_link_warnings");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let mut builder = tar::Builder::new(Vec::new());
    let mut file = tar::Header::new_gnu();
    file.set_size(5);
    file.set_mode(0o644);
    file.set_cksum();
    builder.append_data(&mut file, "a.txt", &b"hello"[..]).unwrap();
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    link.set_mode(0o777);
    builder.append_link(&mut link, "s.txt", "a.txt").unwrap();
    let archive = dir.join("links.tar");
    fs::write(&archive, builder.into_inner().unwrap()).unwrap();

    let out = dir.join("out");
    let output = Command::new(env!("CARGO_BIN_EXE_openarchive"))
        .args(["x", archive.to_str().unwrap(), "-o", out.to_str().unwrap(), "-mmt=1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Warning: skipped symbolic link s.txt -> a.txt"), "{stderr}");
    assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"hello");
    assert!(fs::symlink_metadata(out.join("s.txt")).is_err());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_tar_xz_roundtrip_single_and_multithreaded() {
    let dir = PathBuf::from("test_tar_xz");
    let _ = fs::remove_dir_all(&dir);
    let src = dir.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    // 8 MB of mildly compressible data: several 3 MiB blocks at -mx=1.
    let big: Vec<u8> = (0..8 * 1024 * 1024u32).map(|i| ((i / 7) % 251) as u8).collect();
    fs::write(src.join("big.bin"), &big).unwrap();
    fs::write(src.join("sub/small.txt"), b"small").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");
    for (name, level, threads) in [
        ("st.tar.xz", "-mx=1", "-mmt=1"),
        ("mt.tar.xz", "-mx=1", "-mmt=4"),
        ("store.txz", "-mx=0", "-mmt=1"),
    ] {
        let archive = dir.join(name);
        let created = Command::new(exe)
            .current_dir(&dir)
            .args(["a", name, "src", level, threads])
            .output()
            .unwrap();
        assert!(created.status.success(), "{name}: {}", String::from_utf8_lossy(&created.stderr));
        // xz stream magic.
        assert_eq!(&fs::read(&archive).unwrap()[..6], &[0xFD, b'7', b'z', b'X', b'Z', 0x00], "{name}");

        let tested = Command::new(exe).args(["t", archive.to_str().unwrap()]).output().unwrap();
        assert!(tested.status.success(), "{name}: test");

        let out = dir.join(format!("out-{name}"));
        let extracted = Command::new(exe)
            .args(["x", archive.to_str().unwrap(), "-o", out.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(extracted.status.success(), "{name}: extract");
        assert_eq!(fs::read(out.join("src/big.bin")).unwrap(), big, "{name}");
        assert_eq!(fs::read(out.join("src/sub/small.txt")).unwrap(), b"small", "{name}");
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_misnamed_archive_is_opened_by_content() {
    let dir = PathBuf::from("test_misnamed_archive");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/a.txt"), b"by content").unwrap();

    let exe = env!("CARGO_BIN_EXE_openarchive");
    let created = Command::new(exe).current_dir(&dir).args(["a", "real.7z", "src"]).output().unwrap();
    assert!(created.status.success());
    fs::rename(dir.join("real.7z"), dir.join("misnamed.zip")).unwrap();

    let listed = Command::new(exe).args(["l", dir.join("misnamed.zip").to_str().unwrap()]).output().unwrap();
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("Type = 7z"));

    let out = dir.join("out");
    let extracted = Command::new(exe)
        .args(["x", dir.join("misnamed.zip").to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(extracted.status.success(), "{}", String::from_utf8_lossy(&extracted.stderr));
    assert_eq!(fs::read(out.join("src/a.txt")).unwrap(), b"by content");

    let _ = fs::remove_dir_all(&dir);
}

// ─── Existing files (OA-9) ──────────────────────────────────────────────────

/// Creates `<dir>/a.zip` holding `src/a.txt` = "new", and returns a function
/// that extracts it into `<dir>/out` (where `src/a.txt` = "old") with
/// `switches`, returning (success, stdout, stderr).
fn overwrite_fixture(dir: &Path) -> impl Fn(&[&str]) -> (bool, String, String) + '_ {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/a.txt"), b"new").unwrap();
    let exe = env!("CARGO_BIN_EXE_openarchive");
    assert!(Command::new(exe).current_dir(dir).args(["a", "a.zip", "src"]).output().unwrap().status.success());

    move |switches: &[&str]| {
        let out = dir.join("out");
        let _ = fs::remove_dir_all(&out);
        fs::create_dir_all(out.join("src")).unwrap();
        fs::write(out.join("src/a.txt"), b"old").unwrap();
        let output = Command::new(exe)
            .args(["x", dir.join("a.zip").to_str().unwrap(), "-o", out.to_str().unwrap()])
            .args(switches)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[test]
fn test_existing_files_without_a_terminal_are_kept_with_a_warning() {
    let dir = PathBuf::from("test_ow_default");
    let extract = overwrite_fixture(&dir);
    // No terminal on stdin (as in scripts): must not block, keeps the file.
    let (ok, stdout, stderr) = extract(&[]);
    assert!(ok, "{stderr}");
    assert!(stderr.contains("already exists; kept it"), "{stderr}");
    assert!(stdout.contains("Existing files kept: 1"), "{stdout}");
    assert_eq!(fs::read(dir.join("out/src/a.txt")).unwrap(), b"old");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_overwrite_switches() {
    let dir = PathBuf::from("test_ow_switches");
    let extract = overwrite_fixture(&dir);
    let read = |name: &str| fs::read(dir.join("out/src").join(name)).ok();

    for switch in ["-y", "-aoa"] {
        let (ok, _, stderr) = extract(&[switch]);
        assert!(ok && !stderr.contains("Warning"), "{switch}: {stderr}");
        assert_eq!(read("a.txt").unwrap(), b"new", "{switch}");
    }

    let (ok, stdout, stderr) = extract(&["-aos"]);
    assert!(ok && !stderr.contains("Warning"), "-aos: {stderr}");
    assert_eq!(read("a.txt").unwrap(), b"old");
    assert!(stdout.contains("Existing files kept: 1"), "{stdout}");

    let (ok, stdout, _) = extract(&["-aou"]);
    assert!(ok);
    assert_eq!(read("a.txt").unwrap(), b"old");
    assert_eq!(read("a (1).txt").unwrap(), b"new");
    assert!(stdout.contains("Extracted under a new name: 1"), "{stdout}");

    // An unknown -ao mode (7-Zip's -aot renames the existing file) is an
    // error, not silently ignored.
    let (ok, _, stderr) = extract(&["-aot"]);
    assert!(!ok);
    assert!(stderr.contains("unsupported switch -aot"), "{stderr}");
    assert_eq!(read("a.txt").unwrap(), b"old");

    let _ = fs::remove_dir_all(&dir);
}

/// Runs the CLI in `dir` and returns (exit code, stdout, stderr).
fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_openarchive")).current_dir(dir).args(args).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn wildcard_fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/sub")).unwrap();
    for file in ["src/a.txt", "src/b.log", "src/sub/c.txt", "src/sub/d.log"] {
        fs::write(dir.join(file), file.as_bytes()).unwrap();
    }
    dir
}

#[test]
fn test_add_with_wildcards_and_recurse() {
    let dir = wildcard_fixture("test_wild_add");
    let root = dir.to_str().unwrap();
    let extracted = |archive: &str| {
        let out = format!("{root}/out_{archive}");
        let (code, _, stderr) = run_in(&dir, &["x", archive, &format!("-oout_{archive}")]);
        assert_eq!(code, 0, "{stderr}");
        files_under(&out)
    };

    // The shell does not expand `*` on Windows, so the CLI does.
    let (code, _, stderr) = run_in(&dir, &["a", "top.7z", "src/*.txt"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(extracted("top.7z"), ["a.txt"]);

    let (code, _, stderr) = run_in(&dir, &["a", "deep.zip", "src/*.txt", "-r"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(extracted("deep.zip"), ["a.txt", "sub/c.txt"]);

    // Folders are always added in full, as before.
    let (code, _, stderr) = run_in(&dir, &["a", "all.tar", "src", "-r-"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(extracted("all.tar"), ["src/a.txt", "src/b.log", "src/sub/c.txt", "src/sub/d.log"]);

    // A name that matches nothing: archive still written, warning, exit 1.
    let (code, _, stderr) = run_in(&dir, &["a", "some.7z", "src/*.log", "src/*.zip", "missing.txt"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("No file matches src/*.zip"), "{stderr}");
    assert!(stderr.contains("No file matches missing.txt"), "{stderr}");
    assert_eq!(extracted("some.7z"), ["b.log"]);

    // Nothing at all to add: error, no archive.
    let (code, _, stderr) = run_in(&dir, &["a", "none.7z", "*.rar"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("Nothing to add"), "{stderr}");
    assert!(!dir.join("none.7z").exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_extract_list_and_test_with_wildcards() {
    let dir = wildcard_fixture("test_wild_extract");
    let root = dir.to_str().unwrap();
    let (code, _, stderr) = run_in(&dir, &["a", "arc.7z", "src"]);
    assert_eq!(code, 0, "{stderr}");

    let extract = |args: &[&str]| {
        let _ = fs::remove_dir_all(dir.join("out"));
        let mut all = vec!["x", "arc.7z", "-oout"];
        all.extend_from_slice(args);
        let (code, _, stderr) = run_in(&dir, &all);
        assert_eq!(code, 0, "{args:?}: {stderr}");
        files_under(&format!("{root}/out"))
    };
    assert!(extract(&["*.txt"]).is_empty(), "no .txt at the root of the archive");
    assert_eq!(extract(&["*.txt", "-r"]), ["src/a.txt", "src/sub/c.txt"]);
    assert_eq!(extract(&["src/*.log"]), ["src/b.log"]);
    assert_eq!(extract(&["src/s?b"]), ["src/sub/c.txt", "src/sub/d.log"]);
    // Plain names are exact paths, even with -r.
    assert_eq!(extract(&["src/a.txt", "-r"]), ["src/a.txt"]);
    assert!(extract(&["a.txt", "-r"]).is_empty());

    // `e` flattens what the pattern selects.
    let _ = fs::remove_dir_all(dir.join("flat"));
    let (code, _, stderr) = run_in(&dir, &["e", "arc.7z", "-oflat", "*.log", "-r"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(files_under(&format!("{root}/flat")), ["b.log", "d.log"]);

    let (code, stdout, _) = run_in(&dir, &["l", "arc.7z", "*.log", "-r"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("b.log") && stdout.contains("d.log"), "{stdout}");
    assert!(!stdout.contains("a.txt") && !stdout.contains("c.txt"), "{stdout}");
    assert!(stdout.contains("2 files, 0 folders"), "{stdout}");

    let (code, _, stderr) = run_in(&dir, &["t", "arc.7z", "*.log", "-r"]);
    assert_eq!(code, 0, "{stderr}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_password_problems_are_reported_plainly() {
    let dir = wildcard_fixture("test_cli_passwords");
    for archive in ["enc.7z", "enc.zip"] {
        let (code, _, stderr) = run_in(&dir, &["a", archive, "src", "-psecret"]);
        assert_eq!(code, 0, "{stderr}");

        for command in ["t", "x"] {
            let (code, _, stderr) = run_in(&dir, &[command, archive, "-oout"]);
            assert_eq!(code, 2, "{archive} {command} without password: {stderr}");
            assert!(stderr.contains("Password required: use -p"), "{archive} {command}: {stderr}");

            let (code, _, stderr) = run_in(&dir, &[command, archive, "-oout", "-pwrong"]);
            assert_eq!(code, 2, "{archive} {command} wrong password: {stderr}");
            assert!(stderr.contains("Wrong password"), "{archive} {command}: {stderr}");

            assert!(!stderr.contains("MaybeBadPassword") && !stderr.contains("PasswordRequired"), "{stderr}");
        }
        let (code, _, stderr) = run_in(&dir, &["t", archive, "-psecret"]);
        assert_eq!(code, 0, "{archive} right password: {stderr}");
    }

    // Other fatal errors keep their message and also exit with 2.
    let (code, _, stderr) = run_in(&dir, &["t", "missing.7z"]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.starts_with("Error: "), "{stderr}");

    let _ = fs::remove_dir_all(&dir);
}
