#![deny(clippy::unwrap_used, clippy::expect_used)]
use std::io::Read;

use zerolog_uring::Rotation;
#[test]
fn sync_writes_use_current_descriptor() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("zl-sync-rotation-{}", std::process::id()));
    let mut target = Rotation::open(&path, 1, 1, false)?;
    target.write_all_sync(b"before")?;
    assert_eq!(target.current_size(), 6);
    assert_eq!(std::fs::read(&path)?, b"before");
    std::fs::rename(&path, path.with_extension("old"))?;
    target.begin();
    target.complete_step(-libc::ENOENT)?;
    target.complete_step(0)?;
    assert!(target.complete_step(-libc::EIO).is_err());
    target.write_all_sync(b"-retry")?;
    assert_eq!(std::fs::read(path.with_extension("old"))?, b"before-retry");
    let replacement = std::fs::File::create(&path)?;
    target.opened(replacement.into())?;
    target.write_all_sync(b"new")?;
    assert_eq!(target.current_size(), 3);
    let mut contents = String::new();
    std::fs::File::open(&path)?.read_to_string(&mut contents)?;
    assert_eq!(contents, "new");
    std::fs::remove_file(path.with_extension("old"))?;
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn sync_rotation_obeys_size_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-sync-boundary-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let mut target = Rotation::open(&base, 4, 1, true)?;
    target.write_all_sync(b"one")?;
    target.rotate_sync()?;
    assert_eq!(std::fs::read(&base)?, b"one");
    assert!(!directory.join("events.1").exists());
    target.write_all_sync(b"\n")?;
    target.rotate_sync()?;
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"one\n");
    assert_eq!(std::fs::read(&base)?, b"");
    assert_eq!(target.current_size(), 0);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
