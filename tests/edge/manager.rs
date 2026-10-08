//! The free functions in `manager` (`create_mmap`, `load_mmap`,
//! `write_mmap`, `update_region`, `flush`, `copy_mmap`, `delete_mmap`)
//! and the async surface, driven both by tokio and by a minimal
//! hand-written executor to show that no runtime is assumed.

use std::fs;
use std::io::ErrorKind;

use mmap_io::{
    copy_mmap, create_mmap, delete_mmap, flush, load_mmap, update_region, write_mmap, MmapIoError,
    MmapMode,
};

use crate::common::{pattern, read_file, tmp_path};

fn io_kind<T: std::fmt::Debug>(r: Result<T, MmapIoError>) -> ErrorKind {
    match r {
        Err(MmapIoError::Io(e)) => e.kind(),
        other => panic!("expected Io, got {other:?}"),
    }
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn create_and_load_in_every_mode() {
    let path = tmp_path("mgr.bin");
    let m = create_mmap(&path, 1234).unwrap();
    assert_eq!((m.len(), m.mode()), (1234, MmapMode::ReadWrite));
    update_region(&m, 0, b"via manager").unwrap();
    flush(&m).unwrap();
    drop(m);

    for mode in [MmapMode::ReadOnly, MmapMode::ReadWrite] {
        let m = load_mmap(&path, mode).unwrap();
        assert_eq!(m.mode(), mode);
        assert_eq!(m.as_slice(0, 11).unwrap(), b"via manager");
    }
    #[cfg(feature = "cow")]
    assert_eq!(
        load_mmap(&path, MmapMode::CopyOnWrite).unwrap().mode(),
        MmapMode::CopyOnWrite
    );
    #[cfg(not(feature = "cow"))]
    assert!(matches!(
        load_mmap(&path, MmapMode::CopyOnWrite),
        Err(MmapIoError::InvalidMode(_))
    ));

    assert!(matches!(
        create_mmap(&path, 0),
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        1234,
        "rejected create truncated"
    );
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn free_update_and_flush_follow_the_method_contracts() {
    let path = tmp_path("free.bin");
    let m = create_mmap(&path, 10).unwrap();
    assert!(matches!(
        update_region(&m, 5, b"123456"),
        Err(MmapIoError::OutOfBounds {
            offset: 5,
            len: 6,
            total: 10
        })
    ));
    update_region(&m, 10, b"").unwrap();
    flush(&m).unwrap();
    drop(m);
    let ro = load_mmap(&path, MmapMode::ReadOnly).unwrap();
    assert!(matches!(
        update_region(&ro, 0, b"x"),
        Err(MmapIoError::InvalidMode(_))
    ));
    flush(&ro).unwrap();
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn write_mmap_writes_through_the_page_cache() {
    let path = tmp_path("write.bin");
    drop(create_mmap(&path, 100).unwrap());
    write_mmap(&path, 90, b"0123456789").unwrap();
    assert_eq!(&read_file(&path)[90..], b"0123456789");
    write_mmap(&path, 100, b"").unwrap();
    assert!(matches!(
        write_mmap(&path, 91, b"0123456789"),
        Err(MmapIoError::OutOfBounds {
            offset: 91,
            len: 10,
            total: 100
        })
    ));
    assert!(matches!(
        write_mmap(&path, u64::MAX, b"x"),
        Err(MmapIoError::OutOfBounds { .. })
    ));
    assert_eq!(&read_file(&path)[90..], b"0123456789");

    let missing = path.sibling("missing.bin");
    assert_eq!(io_kind(write_mmap(&missing, 0, b"x")), ErrorKind::NotFound);
    assert!(!missing.exists());

    let empty = path.sibling("empty.bin");
    fs::write(&empty, b"").unwrap();
    assert!(matches!(
        write_mmap(&empty, 0, b"x"),
        Err(MmapIoError::ResizeFailed(_))
    ));
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn copy_mmap_copies_bytes_including_unflushed_writes() {
    let path = tmp_path("src.bin");
    let m = create_mmap(&path, 5000).unwrap();
    m.update_region(0, &pattern(5000, 1)).unwrap();
    // No flush: the page cache already holds the bytes.
    let dst = path.sibling("dst.bin");
    copy_mmap(&*path, dst.as_path()).unwrap();
    assert_eq!(read_file(&dst), pattern(5000, 1));

    // Overwrites an existing, longer destination completely.
    fs::write(&dst, vec![9u8; 9000]).unwrap();
    copy_mmap(&*path, dst.as_path()).unwrap();
    assert_eq!(read_file(&dst), pattern(5000, 1));

    assert_eq!(
        io_kind(copy_mmap(path.sibling("nope.bin"), dst.clone())),
        ErrorKind::NotFound
    );
    assert_eq!(
        io_kind(copy_mmap(
            path.to_path_buf(),
            path.sibling("no/such/dir/x.bin")
        )),
        ErrorKind::NotFound
    );
    drop(m);
}

#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn delete_mmap_removes_the_file_and_reports_errors() {
    let path = tmp_path("del.bin");
    drop(create_mmap(&path, 10).unwrap());
    delete_mmap(&path).unwrap();
    assert!(!path.exists());
    assert_eq!(io_kind(delete_mmap(&path)), ErrorKind::NotFound);
    // A directory is not a mapping file.
    assert!(matches!(delete_mmap(path.dir()), Err(MmapIoError::Io(_))));
    assert!(path.dir().is_dir());
}

/// On Unix a mapped file can be unlinked; the mapping stays readable
/// and writable until it is dropped.
#[cfg(unix)]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn delete_while_mapped_keeps_the_mapping_alive_on_unix() {
    let path = tmp_path("del_mapped.bin");
    let m = create_mmap(&path, 4096).unwrap();
    m.update_region(0, b"still here").unwrap();
    delete_mmap(&path).unwrap();
    assert!(!path.exists());
    assert_eq!(m.as_slice(0, 10).unwrap(), b"still here");
    m.update_region(0, b"STILL").unwrap();
    m.flush().unwrap();
    m.resize(8192).unwrap();
    assert_eq!(m.as_slice(0, 5).unwrap(), b"STILL");
}

/// On Windows a file with a mapped view cannot be deleted. The call
/// must fail cleanly and leave the mapping usable.
#[cfg(windows)]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn delete_while_mapped_fails_cleanly_on_windows() {
    let path = tmp_path("del_mapped.bin");
    let m = create_mmap(&path, 4096).unwrap();
    m.update_region(0, b"still here").unwrap();
    match delete_mmap(&path) {
        Err(MmapIoError::Io(_)) => {
            assert!(path.exists());
            assert_eq!(m.as_slice(0, 10).unwrap(), b"still here");
        }
        Ok(()) => {
            // POSIX delete semantics on newer NTFS: the name is gone but
            // the mapping must stay valid.
            assert_eq!(m.as_slice(0, 10).unwrap(), b"still here");
        }
        Err(e) => panic!("unexpected error {e}"),
    }
}

/// Drive a future to completion on the current thread with a waker
/// that unparks it. Proves the async API needs no particular runtime.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(feature = "async")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn async_methods_work_without_a_runtime() {
    use mmap_io::manager::r#async::{copy_mmap_async, create_mmap_async, delete_mmap_async};
    let path = tmp_path("async.bin");
    let m = block_on(create_mmap_async(&*path, 4096)).unwrap();
    block_on(m.update_region_async(100, b"async")).unwrap();
    assert_eq!(m.pending_bytes(), 0, "update_region_async flushes");
    block_on(m.flush_async()).unwrap();
    block_on(m.flush_range_async(0, 4096)).unwrap();
    let dst = path.sibling("async_copy.bin");
    block_on(copy_mmap_async(path.to_path_buf(), dst.clone())).unwrap();
    assert_eq!(&read_file(&dst)[100..105], b"async");
    block_on(delete_mmap_async(&dst)).unwrap();
    assert!(!dst.exists());
}

#[cfg(feature = "async")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_error_paths() {
    use mmap_io::manager::r#async::{copy_mmap_async, create_mmap_async, delete_mmap_async};
    let path = tmp_path("async_err.bin");

    assert!(matches!(
        create_mmap_async(&*path, 0).await,
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(!path.exists(), "zero-size async create touched the file");
    assert!(matches!(
        create_mmap_async(&*path, u64::MAX).await,
        Err(MmapIoError::ResizeFailed(_))
    ));
    assert!(matches!(
        create_mmap_async(path.sibling("no/dir/x.bin"), 10).await,
        Err(MmapIoError::Io(_))
    ));

    let m = create_mmap_async(&*path, 100).await.unwrap();
    assert!(matches!(
        m.update_region_async(99, b"ab").await,
        Err(MmapIoError::OutOfBounds {
            offset: 99,
            len: 2,
            total: 100
        })
    ));
    assert!(matches!(
        m.flush_range_async(50, 51).await,
        Err(MmapIoError::OutOfBounds { .. })
    ));
    m.flush_range_async(u64::MAX, 0).await.unwrap();
    m.update_region_async(0, b"").await.unwrap();
    drop(m);

    let ro = mmap_io::MemoryMappedFile::open_ro(&path).unwrap();
    assert!(matches!(
        ro.update_region_async(0, b"x").await,
        Err(MmapIoError::InvalidMode(_))
    ));
    ro.flush_async().await.unwrap();
    ro.flush_range_async(0, 100).await.unwrap();
    assert!(matches!(
        ro.flush_range_async(0, 101).await,
        Err(MmapIoError::OutOfBounds { .. })
    ));
    drop(ro);

    let missing = path.sibling("missing.bin");
    assert!(matches!(
        copy_mmap_async(missing.clone(), path.sibling("c.bin")).await,
        Err(MmapIoError::Io(ref e)) if e.kind() == ErrorKind::NotFound
    ));
    assert!(matches!(
        delete_mmap_async(&missing).await,
        Err(MmapIoError::Io(ref e)) if e.kind() == ErrorKind::NotFound
    ));
}

#[cfg(feature = "async")]
#[cfg_attr(miri, ignore = "FFI mmap syscalls are not supported by Miri")]
#[test]
fn many_concurrent_async_writes_land() {
    let path = tmp_path("async_many.bin");
    let m = create_mmap(&path, 64 * 100).unwrap();
    let futs: Vec<_> = (0..100u64)
        .map(|i| {
            let m = m.clone();
            async move { m.update_region_async(i * 64, &[i as u8; 64]).await }
        })
        .collect();
    for f in futs {
        block_on(f).unwrap();
    }
    for i in 0..100u64 {
        assert_eq!(m.as_slice(i * 64, 64).unwrap(), &[i as u8; 64][..]);
    }
}

#[test]
fn block_on_helper_handles_ready_futures() {
    assert_eq!(block_on(async { 7 }), 7);
}
