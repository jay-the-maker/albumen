//! M6 diagnosis (Windows only): does the index name go missing during a
//! replace, and under which rename call? A reader opens the name in a loop
//! while a writer replaces it; reader errors are counted by OS error code.
//!
//! Garage PC, 2026-10-30: MoveFileExW(REPLACE_EXISTING) never left the name
//! missing (0 reader errors) but was refused with ERROR_ACCESS_DENIED (5)
//! whenever a reader had the target open, even with FILE_SHARE_DELETE;
//! `dest::rename` then falls back to `std::fs::rename`, and during that
//! rename readers got ERROR_FILE_NOT_FOUND (495 of 1356 reads). The two-slot
//! layout in `index.rs` never renames over the newest file, so that window
//! no longer reaches a reader.
#![cfg(windows)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn run(label: &str, rename: fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("library.idx");
    std::fs::write(&file, vec![b'x'; 40_000]).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(Mutex::new(BTreeMap::<String, u64>::new()));
    let reads = Arc::new(Mutex::new(0u64));
    let reader = {
        let (stop, errors, reads, file) =
            (stop.clone(), errors.clone(), reads.clone(), file.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                *reads.lock().unwrap() += 1;
                if let Err(err) = std::fs::read(&file) {
                    let key = format!("{:?}/{:?}", err.raw_os_error(), err.kind());
                    *errors.lock().unwrap().entry(key).or_default() += 1;
                }
            }
        })
    };
    let mut renames = 0u64;
    let mut rename_errors = BTreeMap::<String, u64>::new();
    let began = Instant::now();
    let mut n = 0u64;
    while began.elapsed() < Duration::from_secs(3) {
        n += 1;
        let partial = dir.path().join(format!("library.idx.partial-{n}"));
        std::fs::write(&partial, vec![b'y'; 40_000]).unwrap();
        match rename(&partial, &file) {
            Ok(()) => renames += 1,
            Err(err) => {
                *rename_errors
                    .entry(format!("{:?}", err.raw_os_error()))
                    .or_default() += 1;
                let _ = std::fs::remove_file(&partial);
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    println!(
        "M6 DIAG {label} renames={renames} rename_errors={rename_errors:?} reads={} reader_errors={:?}",
        reads.lock().unwrap(),
        errors.lock().unwrap()
    );
}

#[test]
#[ignore = "diagnosis, 12 s: cargo test --test index_rename_diag -- --ignored --nocapture"]
fn where_the_name_goes_missing() {
    use albumen_win::{move_file_ex, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
    run("movefileex_replace_writethrough", |a, b| {
        move_file_ex(a, b, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
    });
    run("movefileex_replace", |a, b| {
        move_file_ex(a, b, MOVEFILE_REPLACE_EXISTING)
    });
    run("std_fs_rename", |a, b| std::fs::rename(a, b));
    run("albumen_dest_rename", |a, b| {
        albumen_ingest::dest::rename(a, b).map_err(|e| std::io::Error::other(format!("{e:?}")))
    });
}
