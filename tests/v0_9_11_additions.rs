//! Integration tests for the 0.9.11 additions:
//! - `as_slice_bytes` compat shim for the 0.9.6 `as_slice` signature
//! - `for_each_mut_legacy` compat shim for the 0.9.6 closure return shape
//! - `bytes::Bytes` integration (gated on `feature = "bytes"`)
//! - `MmapReader` implementing `io::Read` + `io::Seek`
//! - `AsRawFd` / `AsFd` (Unix) and `AsRawHandle` / `AsHandle` (Windows)

use mmap_io::{MemoryMappedFile, MmapIoError};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("mmap_io_v0_9_11_{}_{}", name, std::process::id()));
    p
}

// ---------------------------------------------------------------------
// as_slice_bytes: 0.9.6 compat shim
// ---------------------------------------------------------------------

#[test]
fn as_slice_bytes_works_on_read_only() {
    let path = tmp_path("asb_ro");
    let _ = fs::remove_file(&path);
    {
        let mmap = MemoryMappedFile::create_rw(&path, 32).expect("seed");
        mmap.update_region(0, b"compat-shim-ro").expect("write");
        mmap.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");

    // Returns &[u8] directly, like 0.9.6.
    let s: &[u8] = ro.as_slice_bytes(0, 14).expect("as_slice_bytes");
    assert_eq!(s, b"compat-shim-ro");

    drop(ro);
    let _ = fs::remove_file(&path);
}

#[test]
fn as_slice_bytes_errors_on_read_write_like_0_9_6() {
    let path = tmp_path("asb_rw");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 32).expect("create_rw");

    let err = mmap
        .as_slice_bytes(0, 8)
        .expect_err("RW must error to match 0.9.6 behavior");
    assert!(matches!(err, MmapIoError::InvalidMode(_)));

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn as_slice_bytes_out_of_bounds_errors() {
    let path = tmp_path("asb_oob");
    let _ = fs::remove_file(&path);
    {
        let mmap = MemoryMappedFile::create_rw(&path, 16).expect("seed");
        mmap.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    let err = ro.as_slice_bytes(0, 999).expect_err("OOB must error");
    assert!(matches!(err, MmapIoError::OutOfBounds { .. }));

    drop(ro);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// for_each_mut_legacy: 0.9.6 compat shim
// ---------------------------------------------------------------------

#[cfg(feature = "iterator")]
#[test]
fn for_each_mut_legacy_clean_iteration_returns_ok_ok() {
    let path = tmp_path("femlcy_ok");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create_rw");

    let result: Result<Result<(), std::io::Error>, MmapIoError> =
        mmap.chunks_mut(1024).for_each_mut_legacy(|offset, chunk| {
            let value = (offset / 1024) as u8;
            chunk.fill(value);
            Ok::<(), std::io::Error>(())
        });

    let outer = result.expect("outer mmap-side Result");
    outer.expect("inner closure-side Result");
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(feature = "iterator")]
#[test]
fn for_each_mut_legacy_propagates_closure_error_as_inner_err() {
    let path = tmp_path("femlcy_err");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 4096).expect("create_rw");

    #[derive(Debug, PartialEq, Eq)]
    struct CallerError(&'static str);

    let result: Result<Result<(), CallerError>, MmapIoError> =
        mmap.chunks_mut(1024).for_each_mut_legacy(|offset, chunk| {
            if offset >= 2048 {
                Err(CallerError("bail at chunk 2"))
            } else {
                chunk.fill(0xAB);
                Ok(())
            }
        });

    let outer = result.expect("outer Result must be Ok");
    let inner_err = outer.expect_err("inner Result must be Err");
    assert_eq!(inner_err, CallerError("bail at chunk 2"));

    drop(mmap);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// bytes::Bytes integration
// ---------------------------------------------------------------------

#[cfg(feature = "bytes")]
#[test]
fn read_bytes_returns_owned_bytes() {
    let path = tmp_path("read_bytes");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create_rw");
    mmap.update_region(0, b"bytes-integration-works")
        .expect("write");
    mmap.flush().expect("flush");

    let b: bytes::Bytes = mmap.read_bytes(0, 23).expect("read_bytes");
    assert_eq!(&b[..], b"bytes-integration-works");

    // The returned Bytes is independent of the mapping lifetime.
    drop(mmap);
    let _ = fs::remove_file(&path);
    // b still alive and valid post-drop:
    assert_eq!(&b[..], b"bytes-integration-works");
}

#[cfg(feature = "bytes")]
#[test]
fn mapped_slice_into_bytes_copies_data() {
    let path = tmp_path("ms_into_bytes");
    let _ = fs::remove_file(&path);
    {
        let mmap = MemoryMappedFile::create_rw(&path, 32).expect("seed");
        mmap.update_region(0, b"slice-to-bytes").expect("write");
        mmap.flush().expect("flush");
    }
    let ro = MemoryMappedFile::open_ro(&path).expect("open_ro");
    let b: bytes::Bytes = {
        let slice = ro.as_slice(0, 14).expect("as_slice");
        bytes::Bytes::from(&slice)
    };
    assert_eq!(&b[..], b"slice-to-bytes");

    drop(ro);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// MmapReader: io::Read + io::Seek
// ---------------------------------------------------------------------

#[test]
fn mmap_reader_reads_to_end() {
    let path = tmp_path("reader_full");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create_rw");
    mmap.update_region(0, b"the-quick-brown-fox-jumps-over-the-lazy-dog")
        .expect("write");
    mmap.flush().expect("flush");

    let mut reader = mmap.reader();
    let mut sink = Vec::new();
    reader.read_to_end(&mut sink).expect("read_to_end");
    assert_eq!(sink.len(), 64);
    assert_eq!(&sink[..43], b"the-quick-brown-fox-jumps-over-the-lazy-dog");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn mmap_reader_seek_from_start_and_current() {
    let path = tmp_path("reader_seek");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 32).expect("create_rw");
    mmap.update_region(0, b"0123456789ABCDEFGHIJKLMNOPQRSTUV")
        .expect("write");
    mmap.flush().expect("flush");

    let mut reader = mmap.reader();
    reader.seek(SeekFrom::Start(10)).expect("seek");
    assert_eq!(reader.position(), 10);

    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).expect("read_exact");
    assert_eq!(&buf, b"ABCD");
    assert_eq!(reader.position(), 14);

    reader.seek(SeekFrom::Current(-2)).expect("seek -2");
    assert_eq!(reader.position(), 12);

    reader.seek(SeekFrom::End(0)).expect("seek end");
    assert_eq!(reader.position(), 32);

    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).expect("read_to_end at EOF");
    assert!(tail.is_empty(), "EOF should yield zero bytes");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[test]
fn mmap_reader_eof_returns_zero() {
    let path = tmp_path("reader_eof");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 4).expect("create_rw");
    mmap.update_region(0, b"abcd").expect("write");
    mmap.flush().expect("flush");

    let mut reader = mmap.reader();
    let mut buf = [0u8; 8];
    let n = reader.read(&mut buf).expect("read");
    assert_eq!(n, 4);
    assert_eq!(&buf[..4], b"abcd");

    let n2 = reader.read(&mut buf).expect("read at EOF");
    assert_eq!(n2, 0);

    drop(mmap);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// AsRawFd / AsFd / AsRawHandle / AsHandle
// ---------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn as_raw_fd_returns_valid_fd() {
    use std::os::fd::AsRawFd;

    let path = tmp_path("asrawfd");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 16).expect("create_rw");
    let fd = mmap.as_raw_fd();
    assert!(fd >= 0, "fd must be non-negative; got {fd}");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn as_fd_returns_borrowed_fd() {
    use std::os::fd::AsFd;

    let path = tmp_path("asfd");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 16).expect("create_rw");
    let _borrowed = mmap.as_fd();
    // Existence proof: trait dispatch resolved correctly.
    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(windows)]
#[test]
fn as_raw_handle_returns_valid_handle() {
    use std::os::windows::io::AsRawHandle;

    let path = tmp_path("asrawhandle");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 16).expect("create_rw");
    let h = mmap.as_raw_handle();
    assert!(!h.is_null(), "handle must not be null");

    drop(mmap);
    let _ = fs::remove_file(&path);
}

#[cfg(windows)]
#[test]
fn as_handle_returns_borrowed_handle() {
    use std::os::windows::io::AsHandle;

    let path = tmp_path("ashandle");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 16).expect("create_rw");
    let _borrowed = mmap.as_handle();
    drop(mmap);
    let _ = fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// Smol compatibility validation: the async surface no longer requires
// tokio. We can drive it from a non-tokio block_on. The test crate
// has tokio as a dev-dep purely for the #[tokio::test] macro
// ergonomics; the production crate has no tokio dep.
// ---------------------------------------------------------------------

#[cfg(feature = "async")]
#[test]
fn async_surface_runs_under_a_non_tokio_executor() {
    // Build a minimal block_on from std primitives. This proves the
    // async methods do NOT require a tokio runtime to drive their
    // futures: a parking-based block_on (the same primitive smol
    // and pollster use) suffices.
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::task::{Context, Poll, Wake, Waker};

        struct ParkWaker(std::thread::Thread);
        impl Wake for ParkWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker: Waker = Arc::new(ParkWaker(std::thread::current())).into();
        let mut ctx = Context::from_waker(&waker);
        let mut fut = Box::pin(future);
        loop {
            match Pin::new(&mut fut).poll(&mut ctx) {
                Poll::Ready(out) => return out,
                Poll::Pending => std::thread::park(),
            }
        }
    }

    let path = tmp_path("smol_compat");
    let _ = fs::remove_file(&path);
    let mmap = MemoryMappedFile::create_rw(&path, 64).expect("create_rw");

    let result = block_on(async {
        mmap.update_region_async(0, b"non-tokio").await?;
        mmap.flush_async().await?;
        Ok::<(), MmapIoError>(())
    });

    result.expect("async surface must work under any runtime");

    let mut buf = [0u8; 9];
    mmap.read_into(0, &mut buf).expect("read_into");
    assert_eq!(&buf, b"non-tokio");

    drop(mmap);
    let _ = fs::remove_file(&path);
}
