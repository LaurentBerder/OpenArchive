# 7-Zip vs OpenArchive
## Windows Performance Benchmark and Technical Investigation

**Benchmark type:** Research-grade  
**Platform:** Windows  
**Scope:** Compression, decompression, archive size, CPU, memory, and scalability  
**Status:** Static source-code analysis and targeted validation completed; full empirical benchmark pending.

---

## 1. Objectives

Evaluate the performance of:

- [7-Zip](https://github.com/ip7z/7zip)
- [OpenArchive](https://github.com/LaurentBerder/OpenArchive)

The investigation aims to:

1. Compare compression and decompression throughput.
2. Compare compression ratios and resulting archive sizes.
3. Measure CPU utilization and memory consumption.
4. Evaluate multithreading efficiency.
5. Investigate performance on large files and collections of small files.
6. Identify architectural bottlenecks in OpenArchive.
7. Establish optimization opportunities for OpenArchive.
8. Produce reproducible benchmark results.

## 2. Architecture Comparison

### 2.1 7-Zip

7-Zip uses a mature C/C++ implementation with native compression engines and architecture-specific optimizations.

Its architecture includes:

- Native archive handlers.
- Native LZMA and LZMA2 implementations.
- Deflate and BZIP2 implementations.
- Multithreaded compression and decompression capabilities.
- Windows-specific optimizations.

Repository: https://github.com/ip7z/7zip

### 2.2 OpenArchive

OpenArchive is implemented in Rust and uses existing Rust ecosystem libraries for archive handling and compression.

Its architecture includes:

- Common archive reader/writer abstractions.
- Format-specific handlers.
- External Rust compression libraries.
- CLI and GUI interfaces.

Important dependencies identified during source inspection:

| Component | Dependency |
|---|---|
| 7z | sevenz-rust2 |
| ZIP | zip |
| Deflate/GZIP | flate2 |
| BZIP2 | bzip2 |
| TAR | tar |
| Directory traversal | walkdir |

Repository: https://github.com/LaurentBerder/OpenArchive

### 2.3 Architectural distinction

7-Zip integrates its compression engines into a mature application architecture.

OpenArchive delegates much of its compression functionality to external Rust libraries.

Consequently, performance differences may originate from:

- Compression algorithm implementations.
- Compression library configuration.
- Buffer management.
- Filesystem operations.
- Parallelization strategies.
- Application-level overhead.

The benchmark must distinguish these factors.

---

## 3. Preliminary Source-Code Findings

### 3.1 7z compression configuration

The initial inspection found that the 7z handler accepted compression options but named the parameter `_options`, so none were propagated. Priority 1 corrected this:

- `-mx={0..9}` reaches the encoder. Level 0 uses the COPY method, matching 7-Zip; levels 1–9 map to LZMA2 presets.
- `-md={size}` reaches the LZMA2 dictionary, including 4 GB (`-md=32` / `-md=4g`), which is clamped to the largest encodable dictionary.
- `-ms=on|off` controls solid block grouping.
- `-mmt` selects the block-worker count and enables multi-threaded LZMA2 for oversized blocks.

Verified from archive metadata: `-mx=0` produces a COPY coder, `-md=32m` produces LZMA2 dictionary properties `LZMA2:25`, and the resulting archives pass `7z t`.

### 3.2 ZIP compression configuration

The initial inspection found that the ZIP handler selected Stored for level 0 but did not pass levels 1–9 to the Deflate encoder. Priority 1 now passes the level to `SimpleFileOptions::compression_level`; a test confirms that level 9 produces a smaller archive than level 1 on varied data. TAR.GZ likewise passes the level to `flate2`, and TAR.BZ2 passes it to `bzip2` (verified through the `BZhN` header).

### 3.3 Parallel processing

The initial inspection found sequential file processing in the archive handlers. That finding has since been addressed by the Priority 2 work:

- 7z compression groups input files into solid blocks and encodes the blocks concurrently with `sevenz-rust2` (`prepare_block` / `push_prepared_block`), appending them in archive order from the writer thread. A single oversized block falls back to multi-threaded LZMA2 inside its own stream.
- 7z extraction decodes on one thread and writes small/medium files through a bounded worker pipeline.
- ZIP compression and extraction use bounded worker pipelines.
- TAR remains sequential because it is a streaming format; it should not be described as parallel.

This does not establish that every underlying dependency is multi-threaded; it describes the application-level pipelines.

### 3.4 Extraction overhead

Extraction performs filesystem operations for individual entries, including:

- Creating parent directories.
- Creating output files.
- Copying archive contents.

For archives containing many small files, filesystem overhead can be significant. Priority 3 removed per-file buffer allocation for small entries by writing decoded data directly, but Windows Defender scans every newly created file; on the 15,382-file fixture this scan cost dominated extraction wall time and masked the code-level savings. These observations are hypotheses for broader performance testing, not measured cross-platform results.

---

## 4. Benchmark Environment

Record the following information for every benchmark session.

| Parameter | Value |
|---|---|
| Operating system | Windows |
| Windows version | Pending |
| CPU model | Pending |
| Physical cores | Pending |
| Logical processors | Pending |
| RAM | Pending |
| Storage type | Pending |
| Storage model | Pending |
| 7-Zip version | Pending |
| OpenArchive version | Pending |
| Rust version | Pending |
| Build configuration | Release |
| Filesystem | NTFS |
| Benchmark date | Pending |

### 4.1 Hardware collection

Run the following PowerShell commands:

```powershell
Get-CimInstance Win32_Processor |
    Select-Object Name, NumberOfCores, NumberOfLogicalProcessors

Get-CimInstance Win32_ComputerSystem |
    Select-Object TotalPhysicalMemory

Get-ComputerInfo |
    Select-Object WindowsProductName, WindowsVersion
```

---

## 5. Benchmark Datasets

Five datasets should be used.

| Dataset | Description | Primary bottleneck |
|---|---|---|
| A | Large text files, 1–4 GB | Compression engine |
| B | Source code repository, 100k+ files | Filesystem overhead |
| C | Random binary files, 1–4 GB | CPU and memory throughput |
| D | JPEG/MP4 collections, 1–4 GB | Incompressible data handling |
| E | Mixed workload, 10k–50k files | Real-world performance |

All datasets must be generated once and reused across benchmark runs.

Record their original sizes and SHA-256 hashes.

---

## 6. Benchmark Matrix

### 6.1 7z

| Test | 7-Zip | OpenArchive |
|---|---|---|
| Compression | LZMA2 | LZMA2 |
| Decompression | Native decoder | Rust decoder |
| Compression level | Configurable | Current default |
| Multithreading | Configurable | Current implementation |
| Solid compression | Configurable | Current implementation |

### 6.2 ZIP

| Test | 7-Zip | OpenArchive |
|---|---|---|
| Compression | Deflate | Deflate |
| Decompression | Native decoder | Rust decoder |
| Compression level | Configurable | Current default |
| Multithreading | Configurable | Current implementation |

### 6.3 TAR

Compare archive creation and extraction.

### 6.4 TAR.GZ and TAR.BZ2

Compare the underlying compression and decompression performance, as well as archive overhead.

### 6.5 Cross-extraction

Test interoperability in both directions:

1. Create with 7-Zip, extract with OpenArchive.
2. Create with OpenArchive, extract with 7-Zip.

Verify extracted contents using SHA-256 hashes.

---

## 7. Benchmark Commands

### 7.1 7-Zip compression

Single-thread compression:

```powershell
7z.exe a -t7z -m0=lzma2 -mx=5 -mmt=1 `
    archive.7z .\dataset\
```

Multithread compression:

```powershell
7z.exe a -t7z -m0=lzma2 -mx=5 -mmt=on `
    archive.7z .\dataset\
```

### 7.2 OpenArchive compression

```powershell
openarchive.exe a archive.7z .\dataset\
```

### 7.3 ZIP compression

7-Zip:

```powershell
7z.exe a -tzip -mm=Deflate -mx=5 -mmt=on `
    archive.zip .\dataset\
```

OpenArchive:

```powershell
openarchive.exe a archive.zip .\dataset\ -mx=5
```

**Important:** These are application-default comparisons, not guaranteed equivalent compression configurations. OpenArchive propagates `-mx`, `-md`, `-ms`, and `-mmt` to the 7z writer and `-mx` to the ZIP and TAR.GZ/TAR.BZ2 writers, but other library-specific settings may still differ.

The effective settings must be verified before interpreting compression ratios.

---

## 8. Performance Metrics

Collect the following measurements.

| Metric | Unit |
|---|---|
| Compression duration | Seconds |
| Decompression duration | Seconds |
| Compression throughput | MiB/s |
| Decompression throughput | MiB/s |
| Original dataset size | MiB |
| Archive size | MiB |
| Compression ratio | Percentage |
| Peak working set | MiB |
| Total CPU time | Seconds |
| Average CPU utilization | Percentage |
| File count | Count |

### 8.1 Throughput

Compression throughput:

```text
Throughput = Original size / Compression time
```

Decompression throughput:

```text
Throughput = Uncompressed size / Decompression time
```

### 8.2 Compression ratio

```text
Compression ratio = Archive size / Original size * 100
```

Lower values indicate better compression.

### 8.3 Performance improvement

```text
Speedup = Reference execution time / Candidate execution time
```

A speedup greater than 1 means the candidate is faster.

---

## 9. Thread Scaling

Test the following configurations:

| Configuration | Threads |
|---|---:|
| Single-thread | 1 |
| Dual-thread | 2 |
| Quad-thread | 4 |
| Eight-thread | 8 |
| Maximum | All available |

For OpenArchive, test its default behavior and use process affinity where appropriate to investigate CPU utilization.

Measure:

- Throughput.
- CPU utilization.
- Total CPU time.
- Peak memory.
- Scaling efficiency.

Scaling efficiency:

```text
Efficiency = Speedup / Number of threads * 100
```

Note that this metric assumes comparable workloads and a fixed baseline.

---

## 10. Experimental Methodology

Each test should be repeated at least five times.

Report:

- Median.
- Minimum.
- Maximum.
- Standard deviation.

Control the following variables:

1. Use identical input datasets.
2. Use identical storage devices.
3. Avoid concurrent background workloads.
4. Use release builds for OpenArchive.
5. Record application versions.
6. Separate warm-cache and cold-cache experiments.
7. Verify archive integrity after each test.
8. Ensure compression settings are actually applied.
9. Avoid measuring archive deletion and setup time as compression time.
10. Keep antivirus and Windows Defender conditions consistent.

For storage-bound tests, record the effect of Windows file caching rather than assuming every run has identical disk behavior.

---

## 11. Preliminary Performance Assessment

The following table contains source-code-based expectations, not measured results.

| Workload | Expected advantage | Reason |
|---|---|---|
| 7z compression | 7-Zip | Mature native compression engine |
| 7z decompression | 7-Zip | Native implementation and optimizations |
| ZIP compression | 7-Zip | Mature Deflate implementation; level control is no longer an OpenArchive gap |
| ZIP decompression | 7-Zip | Mature implementation |
| TAR processing | Undetermined | Filesystem and I/O dependent |
| Large datasets | 7-Zip likely | Mature engine; OpenArchive now scales through block-parallel compression |
| Small-file collections | Undetermined | Filesystem overhead may dominate |

No numerical performance advantage should be claimed until the benchmark has been executed.

---

## 12. OpenArchive Optimization Roadmap

### Priority 1: Compression configuration [COMPLETED]

- [x] **Propagate compression levels:** Compression levels (`-mx={0..9}`) propagated across 7z, ZIP, TAR.GZ, and TAR.BZ2 writers. 7z level 0 uses COPY; levels 1–9 use LZMA2.
- [x] **Expose LZMA2 presets:** Mapped level (1–9) to LZMA2 presets via `Lzma2Options::from_level` / `from_level_mt`; level 0 stores data with the COPY coder.
- [x] **Configure dictionary sizes:** Exposed `-md={size}` CLI flag (supporting `k`, `m`, `g`, and 7-Zip bit exponents $\le 32$), passed directly to `Lzma2Options::set_dictionary_size`. The 4 GB maximum is mapped to `u32::MAX` and clamped by the encoder.
- [x] **Expose solid compression:** Added `-ms=on` / `-ms=off` switches. Implemented solid stream grouping in 7z via `push_archive_entries` and individual block creation when disabled.
- [x] **Configure thread counts:** Added `-mmt={N|on|off}` CLI switch and exposed `threads` in `CompressionOptions`.
- [x] **Configure ZIP compression levels:** Passed compression level directly to `SimpleFileOptions::compression_level(...)` for Deflated and selected Stored for level 0.

### Priority 2: Parallel processing [COMPLETED]

- [x] **Parallel LZMA2 compression (7z):** Replaced `sevenz-rust` 0.6 with `sevenz-rust2` 0.25. Files are grouped into solid blocks targeting 64 MB; a file that does not fit forms a block of its own. Each block is encoded on a scoped worker thread with `prepare_block` and appended in archive order with `push_prepared_block`. A sliding window limits workers to `N` blocks ahead of the writer, and prepared blocks are capped at 256 MB, so in-flight memory is bounded by the worker count rather than the archive size. `-mmt={N|on|off}` selects the worker count; `-mmt=1` keeps a single solid block. Blocks larger than 256 MB (for example one huge file) take the streaming path with multi-threaded LZMA2 inside their own stream, so they are never buffered whole in memory.
  - **Preliminary validation** (Ryzen 5 7600, 6C/12T, 31.1 GB RAM, NTFS SSD, 7-Zip 26.06): 128 MB random dataset (32 × 4 MB), `-mx=5`, compression time fell from 45.77 s at `-mmt=1` to 7.98 s at `-mmt=8` (5.7×) and 7.55 s at `-mmt=12` (6.1×). On a 128 MB highly compressible text dataset, 4.34 s → 1.01 s (4.3×) with the same archive size. For reference, 7-Zip took 2.76 s → 0.66 s on the text dataset, so OpenArchive reaches about 65% of 7-Zip's throughput at both 1 and 8 threads with similar scaling.
  - **Interoperability:** every generated archive passed `7z t`; 7-Zip extraction hashes matched the source in both directions (OpenArchive → 7-Zip and 7-Zip → OpenArchive); multi-block encrypted archives also round-tripped correctly.
- [x] **Introduce bounded producer/consumer pipelines:** Built bounded producer/consumer channels (`mpsc::sync_channel`) for compression and extraction. ZIP compression batches files by uncompressed size (`min(32 MB, 256 MB / threads)`, at most 256 entries per batch), and a sliding window stops workers from racing more than `worker_count` batches ahead of the merger, so the reorder buffer cannot grow with archive size. In-flight uncompressed batch data is capped at 256 MB; files larger than the batch limit fall back to the streaming sequential writer, so they are never prepared in memory. The 7z extraction pipeline decouples LZMA2 stream decoding from concurrent file writes up to an 8 MB threshold per item.
  - **Preliminary validation:** a 256 MB ZIP dataset (256 × 1 MB random files) passed `7z t` and both extraction hash checks. Peak working set was 9.7 MB at `-mmt=1`, ~341 MB at `-mmt=8`, and 265 MB at `-mmt=12` (batch size shrinks with thread count to stay within the 256 MB in-flight budget).
- [x] **Investigate parallel extraction:** Implemented parallel random-access extraction for ZIP archives using scoped worker threads, independent `ZipArchive` file handles (zero lock contention), and dynamic work distribution via `AtomicUsize`. Wired thread configuration switch (`-mmt`) directly into `ExtractOptions.threads`.
- [x] **Parallelize independent file operations where safe:** Added upfront directory coverage and sequential pre-creation for 7z and ZIP handlers, eliminating directory races and redundant filesystem metadata lookups during parallel file writing. Integrated size-aware buffering (8 KB small reads, 256 KB sequential, 1 MB archive output) across all handlers. TAR remains sequential by design because the format is streamed; it should not be described as parallel.

### Priority 3: Windows I/O [COMPLETED]

- [x] **Profile directory traversal:** Benchmarked creation on a 15,382-file / 31-directory / 15 MB fixture in store mode, where traversal, file open/close, and metadata dominate. Three runs, minimum warm-cache times before → after the I/O changes: ZIP store 1.58 s → 0.99 s, TAR 2.03 s → 1.09 s, 7z COPY 1.90 s → 1.53 s.
- [x] **Reduce unnecessary filesystem operations:** Replaced `path.is_dir()` / `path.is_file()` in traversal loops with `DirEntry::file_type()` (no extra metadata call) and one size lookup from cached metadata. Symlinked entries retain a path-based fallback. Top-level inputs now use a single metadata call instead of separate `is_dir` / `is_file` checks.
- [x] **Investigate larger I/O buffers:** Added size-aware buffers: 8 KB reads for files below 64 KB, 256 KB buffered sequential reads/writes for larger files, and 1 MB output buffers for ZIP/TAR/7z archive writers. Compression inputs opened on Windows use `FILE_FLAG_SEQUENTIAL_SCAN` so the cache manager can tune read-ahead for streaming.
- [x] **Investigate asynchronous I/O where beneficial:** Evaluated but not adopted. Local NTFS reads are already overlapped by OS read-ahead and by the worker pools; compression/decompression remains CPU-bound. An async runtime would add complexity without a demonstrated benefit for these access patterns. Revisit for network or high-latency storage.
- [x] **Optimize handling of small files:** Decompressed entries below 64 KB are written directly instead of allocating a 64 KB `BufWriter` per file; 7z parallel workers write fully decoded buffers directly; compression readers no longer allocate a 64 KB buffer for every tiny file. This removes per-file buffer allocation and an extra copy on the small-file path.
  - **Note:** extraction wall time on a 1,000-file subset was effectively unchanged (31.66 s → 32.86 s) because Windows Defender scans each newly created file, which dominates the measurement. The allocation/copy reductions are real but not visible under that AV overhead.

### Priority 4: Memory management

- Measure allocation frequency.
- Reuse buffers.
- Reduce intermediate allocations.
- Measure peak memory consumption.
- Evaluate memory usage under multithreaded workloads.

Every optimization must be validated against the benchmark suite.

---

## 13. Results

**Status: Pending empirical execution.**

The following tables should be populated with actual measurements.

### 13.1 Compression results

| Dataset | Format | 7-Zip MiB/s | OpenArchive MiB/s | Speedup |
|---|---|---:|---:|---:|
| A | 7z | TBD | TBD | TBD |
| A | ZIP | TBD | TBD | TBD |
| B | 7z | TBD | TBD | TBD |
| B | ZIP | TBD | TBD | TBD |
| C | 7z | TBD | TBD | TBD |
| C | ZIP | TBD | TBD | TBD |
| D | 7z | TBD | TBD | TBD |
| D | ZIP | TBD | TBD | TBD |
| E | 7z | TBD | TBD | TBD |
| E | ZIP | TBD | TBD | TBD |

### 13.2 Decompression results

| Dataset | Format | 7-Zip MiB/s | OpenArchive MiB/s | Speedup |
|---|---|---:|---:|---:|
| A | 7z | TBD | TBD | TBD |
| A | ZIP | TBD | TBD | TBD |
| B | 7z | TBD | TBD | TBD |
| B | ZIP | TBD | TBD | TBD |
| C | 7z | TBD | TBD | TBD |
| C | ZIP | TBD | TBD | TBD |
| D | 7z | TBD | TBD | TBD |
| D | ZIP | TBD | TBD | TBD |
| E | 7z | TBD | TBD | TBD |
| E | ZIP | TBD | TBD | TBD |

### 13.3 Memory results

| Workload | 7-Zip peak RAM | OpenArchive peak RAM |
|---|---:|---:|
| Large-file compression | TBD | TBD |
| Large-file extraction | TBD | TBD |
| Small-file compression | TBD | TBD |
| Small-file extraction | TBD | TBD |

### 13.4 Thread scaling results

| Threads | 7-Zip MiB/s | OpenArchive MiB/s |
|---:|---:|---:|
| 1 | TBD | TBD |
| 2 | TBD | TBD |
| 4 | TBD | TBD |
| 8 | TBD | TBD |
| Maximum | TBD | TBD |

---

## 14. Preliminary Conclusion

The source-code investigation suggests that 7-Zip still has advantages in compression-engine maturity and Windows-specific optimization. OpenArchive now exposes the main compression settings and compresses 7z solid blocks in parallel.

The remaining implementation priority is memory management (Priority 4). The full benchmark matrix remains to be run.

These findings do not establish actual performance ratios.

The empirical benchmark is necessary to determine the size of the performance gap and identify which optimizations offer the greatest practical benefit.

The main engineering conclusion is that Rust itself is not the limiting factor. The quality of the underlying compression implementation and the way it is integrated into OpenArchive are much more important.

---

## 15. References

- [7-Zip GitHub repository](https://github.com/ip7z/7zip)
- [OpenArchive GitHub repository](https://github.com/LaurentBerder/OpenArchive)
- [sevenz-rust2 documentation](https://docs.rs/sevenz-rust2/)
- [ZIP Rust crate documentation](https://docs.rs/zip/)
- [flate2 documentation](https://docs.rs/flate2/)
- [7z format specification](https://www.7-zip.org/7z.html)

---

**Report status:** Source-code investigation completed; Windows empirical benchmark pending.

**Important:** All future performance conclusions must be based on reproducible measurements. Source-code observations and theoretical expectations must remain clearly distinguished from experimentally verified results.