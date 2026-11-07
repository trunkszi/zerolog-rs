use super::*;
#[test]
fn sync_write_retries_interrupted_prefix() -> Result<(), Box<dyn std::error::Error>> {
    let mut target =
        Rotation::from_fd(std::fs::File::options().write(true).open("/dev/null")?.into());
    let mut remaining = &b"bytes"[..];
    let mut calls = 0;
    let result = target.write_sync_with(&mut remaining, |bytes| {
        calls += 1;
        match calls {
            1 => Err(libc::EINTR),
            2 => {
                assert_eq!(bytes, b"bytes");
                Ok(2)
            }
            _ => {
                assert_eq!(bytes, b"tes");
                Err(libc::EAGAIN)
            }
        }
    });
    assert_eq!(result, Err(ZeroLogError::Write(libc::EAGAIN)));
    assert_eq!(target.current_size(), 2);
    assert_eq!(remaining, b"tes");
    assert_eq!(
        target.write_sync_with(&mut remaining, |_| Ok(0)),
        Err(ZeroLogError::Write(libc::EIO))
    );
    target.write_sync_with(&mut remaining, |bytes| Ok(bytes.len()))?;
    assert_eq!(target.current_size(), 5);
    assert!(remaining.is_empty());
    Ok(())
}

#[test]
fn sync_rotation_retries_open_only() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-sync-open-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let mut target = Rotation::open(&base, 4, 2, true)?;
    target.write_all_sync(b"one\n")?;
    let old = target.current_fd();
    let mut interrupted = Vec::new();
    let outcome = target.rotate_sync_with(|rotation| {
        if !interrupted.contains(&rotation.step()) {
            interrupted.push(rotation.step());
            return -libc::EINTR;
        }
        if rotation.step() == RotationStep::Open
            && !base.exists()
            && let Err(error) = std::fs::create_dir(&base)
        {
            return -error.raw_os_error().unwrap_or(libc::EIO);
        }
        rotation.sync_step()
    });
    assert_eq!(outcome, Err(ZeroLogError::Rotation { step: 5, errno: libc::EISDIR }));
    assert_eq!(target.step(), RotationStep::Open);
    assert!(target.reopen_pending());
    assert!(Rc::ptr_eq(&old, &target.current_fd()));
    target.write_all_sync(b"retry\n")?;
    std::fs::remove_dir(&base)?;
    target.rotate_sync()?;
    assert_eq!(target.step(), RotationStep::Idle);
    assert!(!target.reopen_pending());
    assert!(!Rc::ptr_eq(&old, &target.current_fd()));
    assert_eq!(target.current_size(), 0);
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"one\nretry\n");
    assert!(!directory.join("events.2").exists());
    assert_eq!(std::fs::read(base)?, b"");
    assert_eq!(interrupted.len(), 5);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
