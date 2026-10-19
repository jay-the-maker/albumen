// SPDX-License-Identifier: Apache-2.0

//! Generated RAID-like tree. Ignored by `cargo test` so a pull request does
//! not create a million inodes. `.github/workflows/raid-scale.yml` runs it
//! with `--ignored` on `workflow_dispatch` and a weekly schedule. That job
//! must not set `ALBUMEN_RAID_ITEMS` to a smaller count. A local smoke test may.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime};

#[derive(Debug, PartialEq, Eq)]
struct Snap {
    rel: String,
    is_dir: bool,
    len: u64,
    mtime: SystemTime,
    mode: u32,
    xattrs: Vec<(String, Vec<u8>)>,
}

fn xattr_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "albumen.trial"
    } else {
        "user.albumen-trial"
    }
}

fn item_count() -> u64 {
    std::env::var("ALBUMEN_RAID_ITEMS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(1_000_000)
}

fn mark(path: &Path) {
    xattr::set(path, xattr_name(), b"raid").unwrap();
}

fn touch_empty(path: &Path) {
    fs::File::create(path).unwrap();
}

fn sparse(path: &Path, len: u64) {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.set_len(len).unwrap();
    let meta = file.metadata().unwrap();
    assert!(
        meta.blocks() * 512 < len / 2,
        "expected a sparse file at {}, blocks {}",
        path.display(),
        meta.blocks()
    );
}

fn build(root: &Path, items: u64) {
    let wide = root.join("wide");
    fs::create_dir(&wide).unwrap();
    for index in 0..items {
        let path = wide.join(format!("{index:07}.dat"));
        touch_empty(&path);
        if index == 0 {
            mark(&path);
        }
        if index > 0 && index % 100_000 == 0 {
            eprintln!("raid tree: {index} wide files");
        }
    }

    let mut deep = root.join("deep");
    fs::create_dir(&deep).unwrap();
    while deep.as_os_str().len() + 2 < 800 {
        deep.push("d");
        fs::create_dir(&deep).unwrap();
    }
    touch_empty(&deep.join("leaf.txt"));

    let mixed = root.join("mixed");
    fs::create_dir(&mixed).unwrap();
    for name in [
        "still.jpg",
        "still.dng",
        "still.xmp",
        "still.heic",
        "still.mov",
        "still.aae",
        "照片.jpg",
        "фото.jpg",
        "a file.jpg",
        "a,b.jpg",
        "naïve.jpg",
        "photo😀.jpg",
        ".hidden.jpg",
        "empty.jpg",
    ] {
        touch_empty(&mixed.join(name));
    }
    mark(&mixed.join("still.jpg"));

    let huge = root.join("huge");
    fs::create_dir(&huge).unwrap();
    let gib = 1_u64 << 30;
    sparse(&huge.join("one.bin"), gib);
    sparse(&huge.join("two.bin"), gib);
    mark(&huge.join("one.bin"));
    mark(root);
}

fn capture(root: &Path) -> Vec<Snap> {
    let mut out = Vec::new();
    fn push(path: &Path, rel: String, out: &mut Vec<Snap>) {
        let meta = fs::symlink_metadata(path).unwrap();
        let mut names: Vec<_> = xattr::list(path).unwrap().collect();
        names.sort();
        let xattrs = names
            .into_iter()
            .map(|name| {
                let value = xattr::get(path, &name).unwrap().unwrap_or_default();
                (name.to_string_lossy().into_owned(), value)
            })
            .collect();
        out.push(Snap {
            rel,
            is_dir: meta.is_dir(),
            len: meta.len(),
            mtime: meta.modified().unwrap(),
            mode: meta.mode(),
            xattrs,
        });
    }
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<Snap>) {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            let name = name.to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let is_dir = fs::symlink_metadata(&path).unwrap().is_dir();
            push(&path, rel.clone(), out);
            if is_dir {
                walk(&path, &rel, out);
            }
        }
    }
    push(root, String::new(), &mut out);
    walk(root, "", &mut out);
    out
}

fn albumen(args: &[impl AsRef<std::ffi::OsStr>]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_albumen"))
        .args(args)
        .env("ALBUMEN_EXIFTOOL_BATCH", "4096")
        .output()
        .expect("run albumen");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{text}");
    text
}

#[test]
#[ignore = "creates at least one million files; raid-scale.yml passes --ignored"]
fn scan_and_verify_leave_a_generated_tree_unchanged() {
    let items = item_count();
    assert!(items >= 1, "ALBUMEN_RAID_ITEMS must be at least 1");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("raid");
    fs::create_dir(&root).unwrap();
    let started = Instant::now();
    build(&root, items);
    eprintln!("raid tree built in {:.1}s", started.elapsed().as_secs_f64());
    let files = capture(&root).iter().filter(|entry| !entry.is_dir).count();
    assert!(
        files >= items as usize,
        "tree has {files} files, wanted {items}"
    );
    let before = capture(&root);
    let manifest: PathBuf = tmp.path().join("scan.jsonl");
    assert!(!manifest.starts_with(&root));
    albumen(&[
        "scan",
        root.to_str().unwrap(),
        "--manifest",
        manifest.to_str().unwrap(),
    ]);
    albumen(&[
        "verify-manifest",
        "--manifest",
        manifest.to_str().unwrap(),
        "--against",
        root.to_str().unwrap(),
    ]);
    assert_eq!(capture(&root), before);
}
