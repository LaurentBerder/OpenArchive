//! Block-parallel gzip and bzip2 stream compression.
//!
//! The input stream is cut into fixed-size chunks; each chunk is compressed
//! on a worker thread into a complete, independent gzip member (RFC 1952
//! §2.2) or bzip2 stream, and the results are written in order. The output is
//! the plain concatenation of those members, the same layout pigz `-i`,
//! pbzip2 and `cat a.gz b.gz` produce. Standard `gzip -d`, `bzip2 -d`, `tar`,
//! 7-Zip, and the `MultiGzDecoder` / `MultiBzDecoder` readers all accept it.
//!
//! Memory is bounded: at most `max_inflight` chunks exist at once (being
//! filled, queued, compressed, or waiting in the reorder buffer), enforced by
//! a pool of permits that the writer thread returns after each chunk is
//! written.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const MB: u64 = 1024 * 1024;

/// Budget for chunk buffers and encoder state across all workers. Matches
/// the in-flight budget used by the 7z and ZIP writers.
const MAX_INFLIGHT_BYTES: u64 = 256 * MB;

/// Uncompressed size of one gzip member. Each member restarts the 32 KiB
/// Deflate window, so smaller chunks cost ratio; 1 MiB loses well under 0.1%
/// on text while giving enough chunks to balance across workers.
const GZIP_CHUNK_BYTES: usize = 1024 * 1024;

/// Bzip2 compresses independent blocks of `level * 100 kB` anyway, so a chunk
/// of a few blocks loses only the 14-byte stream header/trailer per chunk.
const BZIP2_BLOCKS_PER_CHUNK: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// Deflate level 0..=9.
    Gzip(u32),
    /// Block size 1..=9 (x 100 kB).
    Bzip2(u32),
}

impl Codec {
    fn chunk_size(self) -> usize {
        match self {
            Codec::Gzip(_) => GZIP_CHUNK_BYTES,
            Codec::Bzip2(level) => level.clamp(1, 9) as usize * 100_000 * BZIP2_BLOCKS_PER_CHUNK,
        }
    }

    /// Working memory of one live chunk: the input plus its compressed
    /// output, which is bounded by the input plus a small expansion.
    fn bytes_per_chunk(self) -> u64 {
        2 * self.chunk_size() as u64
    }

    /// Encoder state held by one busy worker. bzip2 uses ~400 kB plus
    /// 8 x block size; miniz_oxide's Deflate state is a few hundred KiB.
    fn encoder_bytes(self) -> u64 {
        match self {
            Codec::Gzip(_) => 512 * 1024,
            Codec::Bzip2(level) => 400_000 + 8 * 100_000 * level.clamp(1, 9) as u64,
        }
    }

    fn compress(self, data: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Codec::Gzip(level) => {
                let mut enc = flate2::write::GzEncoder::new(
                    Vec::with_capacity(data.len() / 2 + 64),
                    flate2::Compression::new(level.min(9)),
                );
                enc.write_all(data)?;
                enc.finish()
            }
            Codec::Bzip2(level) => {
                let mut enc = bzip2::write::BzEncoder::new(
                    Vec::with_capacity(data.len() / 3 + 64),
                    bzip2::Compression::new(level.clamp(1, 9)),
                );
                enc.write_all(data)?;
                enc.finish()
            }
        }
    }
}

/// Peak working memory of a pipeline with `workers` workers: `2w + 1` live
/// chunks (see [`ParallelCompressor::new`]) plus one encoder per worker.
fn pipeline_bytes(codec: Codec, workers: u64) -> u64 {
    (2 * workers + 1) * codec.bytes_per_chunk() + workers * codec.encoder_bytes()
}

/// Number of compression workers for `requested` threads, capped so the
/// whole pipeline fits in the in-flight memory budget.
pub(crate) fn worker_count(codec: Codec, requested: usize) -> usize {
    // pipeline_bytes is linear in w: fixed + w * per_worker.
    let fixed = pipeline_bytes(codec, 0);
    let per_worker = pipeline_bytes(codec, 1) - fixed;
    let cap = (MAX_INFLIGHT_BYTES.saturating_sub(fixed) / per_worker).max(1) as usize;
    requested.max(1).min(cap)
}

type Job = (u64, Vec<u8>);
type Done = (u64, io::Result<Vec<u8>>);

/// A `Write` sink that compresses its input in parallel. Call [`finish`]
/// to flush the last chunk and recover the inner writer; dropping it without
/// finishing abandons the output.
///
/// [`finish`]: ParallelCompressor::finish
pub(crate) struct ParallelCompressor<W: Write + Send + 'static> {
    chunk_size: usize,
    buffer: Vec<u8>,
    next_seq: u64,
    jobs: Option<SyncSender<Job>>,
    permits: Receiver<()>,
    workers: Vec<JoinHandle<()>>,
    writer: Option<JoinHandle<io::Result<W>>>,
}

impl<W: Write + Send + 'static> ParallelCompressor<W> {
    pub(crate) fn new(inner: W, codec: Codec, workers: usize) -> io::Result<Self> {
        let workers = workers.max(1);
        let chunk_size = codec.chunk_size();
        // One chunk being filled by the caller plus up to two per worker
        // (one compressing, one queued or awaiting reorder).
        let max_inflight = 2 * workers + 1;

        let (permit_tx, permits) = mpsc::sync_channel::<()>(max_inflight);
        for _ in 0..max_inflight {
            permit_tx.send(()).expect("permit channel has capacity");
        }

        let (job_tx, job_rx) = mpsc::sync_channel::<Job>(workers);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done_rx) = mpsc::channel::<Done>();

        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let done_tx = done_tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("openarchive-compress-{i}"))
                .spawn(move || compress_worker(codec, job_rx, done_tx))?;
            handles.push(handle);
        }
        drop(done_tx);

        let writer = std::thread::Builder::new()
            .name("openarchive-compress-writer".into())
            .spawn(move || ordered_writer(inner, done_rx, permit_tx))?;

        Ok(Self {
            chunk_size,
            buffer: Vec::with_capacity(chunk_size),
            next_seq: 0,
            jobs: Some(job_tx),
            permits,
            workers: handles,
            writer: Some(writer),
        })
    }

    fn dispatch(&mut self) -> io::Result<()> {
        // Wait for a permit before allocating the next chunk buffer, so the
        // number of live chunks never exceeds `max_inflight`.
        if self.permits.recv().is_err() {
            return Err(self.pipeline_error());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(self.chunk_size));
        let seq = self.next_seq;
        self.next_seq += 1;
        let sent = self
            .jobs
            .as_ref()
            .map(|jobs| jobs.send((seq, chunk)).is_ok())
            .unwrap_or(false);
        if !sent {
            return Err(self.pipeline_error());
        }
        Ok(())
    }

    /// Shut the pipeline down and return the writer thread's error, if any.
    fn pipeline_error(&mut self) -> io::Error {
        match self.shutdown() {
            Err(e) => e,
            Ok(_) => io::Error::other("parallel compressor stopped unexpectedly"),
        }
    }

    fn shutdown(&mut self) -> io::Result<W> {
        self.jobs.take();
        for handle in self.workers.drain(..) {
            if handle.join().is_err() {
                return Err(io::Error::other("compression worker panicked"));
            }
        }
        match self.writer.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| io::Error::other("compression writer panicked"))?,
            None => Err(io::Error::other("parallel compressor already finished")),
        }
    }

    /// Compress the final partial chunk, wait for all output to be written,
    /// and return the inner writer (flushed).
    pub(crate) fn finish(mut self) -> io::Result<W> {
        // An empty input still needs one valid (empty) member: a zero-byte
        // file is not a valid gzip or bzip2 stream.
        if !self.buffer.is_empty() || self.next_seq == 0 {
            self.dispatch()?;
        }
        self.shutdown()
    }
}

impl<W: Write + Send + 'static> Write for ParallelCompressor<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut rest = data;
        while !rest.is_empty() {
            let room = self.chunk_size - self.buffer.len();
            let take = room.min(rest.len());
            self.buffer.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.buffer.len() == self.chunk_size {
                self.dispatch()?;
            }
        }
        Ok(data.len())
    }

    /// Chunks are only emitted when full or on [`finish`](Self::finish);
    /// flushing mid-stream would end a member early and hurt the ratio.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<W: Write + Send + 'static> Drop for ParallelCompressor<W> {
    fn drop(&mut self) {
        if self.writer.is_some() {
            let _ = self.shutdown();
        }
    }
}

fn compress_worker(codec: Codec, jobs: Arc<Mutex<Receiver<Job>>>, done: Sender<Done>) {
    loop {
        let job = {
            let Ok(rx) = jobs.lock() else { return };
            rx.recv()
        };
        let Ok((seq, data)) = job else { return };
        let result = codec.compress(&data);
        drop(data);
        if done.send((seq, result)).is_err() {
            return;
        }
    }
}

fn ordered_writer<W: Write>(
    mut inner: W,
    done: Receiver<Done>,
    permits: SyncSender<()>,
) -> io::Result<W> {
    let mut pending: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut next = 0u64;
    for (seq, result) in done {
        pending.insert(seq, result?);
        while let Some(chunk) = pending.remove(&next) {
            inner.write_all(&chunk)?;
            next += 1;
            // The receiver only disappears once the producer is gone.
            let _ = permits.send(());
        }
    }
    if !pending.is_empty() {
        return Err(io::Error::other("compressed chunk missing from output"));
    }
    inner.flush()?;
    Ok(inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut x: u32 = 12345;
        while out.len() < len {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            // Mix compressible text with noise.
            if (x >> 16).is_multiple_of(4) {
                out.push((x >> 24) as u8);
            } else {
                out.extend_from_slice(b"openarchive ");
            }
        }
        out.truncate(len);
        out
    }

    fn compress(codec: Codec, workers: usize, data: &[u8], write_size: usize) -> Vec<u8> {
        let mut pc = ParallelCompressor::new(Vec::new(), codec, workers).unwrap();
        for piece in data.chunks(write_size.max(1)) {
            pc.write_all(piece).unwrap();
        }
        pc.finish().unwrap()
    }

    fn gunzip(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(data)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn bunzip(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        bzip2::read::MultiBzDecoder::new(data)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn gzip_roundtrip_across_chunk_boundaries() {
        let data = sample(GZIP_CHUNK_BYTES * 5 + 777);
        for workers in [1, 2, 8] {
            for write_size in [512, 100_003, GZIP_CHUNK_BYTES * 3] {
                let gz = compress(Codec::Gzip(6), workers, &data, write_size);
                assert_eq!(&gz[..2], &[0x1f, 0x8b]);
                assert_eq!(gunzip(&gz), data, "workers={workers} write={write_size}");
            }
        }
    }

    #[test]
    fn bzip2_roundtrip_across_chunk_boundaries() {
        let codec = Codec::Bzip2(1);
        let data = sample(codec.chunk_size() * 3 + 4321);
        for workers in [1, 3] {
            let bz = compress(codec, workers, &data, 65_536);
            assert_eq!(&bz[..4], b"BZh1");
            assert_eq!(bunzip(&bz), data, "workers={workers}");
        }
    }

    #[test]
    fn output_is_one_member_per_chunk() {
        let data = sample(GZIP_CHUNK_BYTES * 2 + 1);
        let gz = compress(Codec::Gzip(1), 4, &data, 4096);
        // A single-member decoder sees only the first chunk.
        let mut first = Vec::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_end(&mut first)
            .unwrap();
        assert_eq!(first, &data[..GZIP_CHUNK_BYTES]);
        assert_eq!(gunzip(&gz), data);
    }

    #[test]
    fn empty_input_produces_valid_stream() {
        for codec in [Codec::Gzip(6), Codec::Bzip2(9)] {
            let out = compress(codec, 4, &[], 1);
            assert!(!out.is_empty());
            let decoded = match codec {
                Codec::Gzip(_) => gunzip(&out),
                Codec::Bzip2(_) => bunzip(&out),
            };
            assert!(decoded.is_empty());
        }
    }

    #[test]
    fn output_is_deterministic_regardless_of_workers() {
        let data = sample(GZIP_CHUNK_BYTES * 4 + 99);
        let one = compress(Codec::Gzip(6), 1, &data, 8192);
        let many = compress(Codec::Gzip(6), 6, &data, 8192);
        assert_eq!(one, many);
    }

    #[test]
    fn writer_errors_propagate() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let data = sample(GZIP_CHUNK_BYTES * 20);
        let mut pc = ParallelCompressor::new(Failing, Codec::Gzip(1), 2).unwrap();
        let result = pc.write_all(&data).and_then(|_| pc.finish().map(|_| ()));
        let err = result.expect_err("write to a failing sink must error");
        assert!(err.to_string().contains("disk full"), "{err}");
    }

    #[test]
    fn worker_count_respects_budget() {
        assert_eq!(worker_count(Codec::Gzip(6), 1), 1);
        assert_eq!(worker_count(Codec::Gzip(6), 8), 8);
        assert_eq!(worker_count(Codec::Bzip2(9), 0), 1);
        for codec in [Codec::Gzip(9), Codec::Bzip2(1), Codec::Bzip2(9)] {
            let cap = worker_count(codec, 10_000) as u64;
            assert!(pipeline_bytes(codec, cap) <= MAX_INFLIGHT_BYTES, "{codec:?}");
            assert!(pipeline_bytes(codec, cap + 1) > MAX_INFLIGHT_BYTES, "{codec:?}");
        }
        // Typical desktop thread counts are not capped.
        assert_eq!(worker_count(Codec::Bzip2(9), 12), 12);
    }
}
