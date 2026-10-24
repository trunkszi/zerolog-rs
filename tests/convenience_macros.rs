use std::{
    io::{self, Write},
    sync::{Arc, Mutex, OnceLock},
};

use zerolog::{
    Level, OFF, flush_on, poll, set_flush_buffer_size, set_flush_delay, set_level,
    set_output_writer, stop_polling_thread,
};

struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn capture_output<F: FnOnce()>(level: Level, action: F) -> String {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(8 * 1024);
    flush_on(OFF);
    let buffer = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(level);

    action();
    poll(true).unwrap();

    String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
}

#[test]
fn test_logd() {
    let output = capture_output(Level::DBG, || {
        zerolog::debug!("debug value={}", 42u32);
    });
    assert!(output.contains("DBG"), "output: {output}");
    assert!(output.contains("debug value=42"), "output: {output}");
}

#[test]
fn test_logi() {
    let output = capture_output(Level::DBG, || {
        zerolog::info!("info message={}", "hello");
    });
    assert!(output.contains("INF"), "output: {output}");
    assert!(output.contains("info message=hello"), "output: {output}");
}

#[test]
fn test_logw() {
    let output = capture_output(Level::DBG, || {
        zerolog::warn!("warning count={}", 99u64);
    });
    assert!(output.contains("WRN"), "output: {output}");
    assert!(output.contains("warning count=99"), "output: {output}");
}

#[test]
fn test_loge() {
    let output = capture_output(Level::DBG, || {
        zerolog::error!("error code={}", -1i32);
    });
    assert!(output.contains("ERR"), "output: {output}");
    assert!(output.contains("error code=-1"), "output: {output}");
}

#[test]
fn test_logd_filtered_by_level() {
    let output = capture_output(Level::INF, || {
        zerolog::debug!("should not appear={}", 0u32);
        zerolog::info!("should appear={}", 1u32);
    });
    assert!(!output.contains("should not appear"), "output: {output}");
    assert!(output.contains("should appear=1"), "output: {output}");
}

#[test]
fn test_no_args() {
    let output = capture_output(Level::DBG, || {
        zerolog::info!("plain message no args");
    });
    assert!(output.contains("plain message no args"), "output: {output}");
}

/// A generic call site retains the decoder for each concrete argument layout.
#[test]
fn generic_site_keeps_typed_metadata() {
    fn emit<T: zerolog::EncodeFast>(value: T) {
        zerolog::info!("typed={}", value);
    }
    let output = capture_output(Level::INF, || {
        emit(7u8);
        emit(u64::MAX);
        emit("monomorphized");
    });
    assert!(output.contains("typed=7"), "{output}");
    assert!(output.contains("typed=18446744073709551615"), "{output}");
    assert!(output.contains("typed=monomorphized"), "{output}");
}

/// Each runtime level selects its own metadata at a shared source location.
#[test]
fn runtime_level_is_not_cached() {
    let output = capture_output(Level::TRC, || {
        for level in [Level::INF, Level::ERR] {
            zerolog::log!(level, "dynamic level");
        }
    });
    let lines: Vec<_> = output.lines().collect();
    assert_eq!(lines.len(), 2, "{output}");
    assert!(lines[0].contains("INF"), "{output}");
    assert!(lines[1].contains("ERR"), "{output}");
}

/// Positional reuse and captures evaluate producer expressions only once.
#[test]
fn positional_capture_and_trailing_comma() {
    let mut evaluated = 0;
    let output = capture_output(Level::INF, || {
        zerolog::info!("a={}", 1,);
        zerolog::info!(
            "{1} {0} {0}",
            {
                evaluated += 1;
                10
            },
            20
        );
        let x = 42;
        zerolog::info!("x={x} other={}", 7);
    });
    assert!(output.contains("a=1"), "{output}");
    assert!(output.contains("20 10 10"), "{output}");
    assert!(output.contains("x=42 other=7"), "{output}");
    assert_eq!(evaluated, 1);
}

/// Borrowed and owned text serialize their contents, including Unicode precision.
#[test]
fn borrowed_values_and_unicode_precision() {
    use std::borrow::Cow;
    let output = capture_output(Level::INF, || {
        let text = String::from("string");
        let slice = "slice";
        let number = 42u64;
        zerolog::info!(
            "{} {} {} {} {}",
            &text,
            &slice,
            &number,
            Cow::Borrowed("borrowed"),
            Cow::<'_, str>::Owned("owned".into())
        );
        zerolog::info!("unicode={:.1}", "éabc");
        zerolog::info!("scientific={:.2e}/{:.2E}", 1234.0f64, 1234.0f64);
        zerolog::info!("repeat={1:x}/{0:.1}/{1:x}", "éabc", 255u64);
    });
    assert!(output.contains("string slice 42 borrowed owned"), "{output}");
    assert!(output.contains("unicode=é"), "{output}");
    assert!(output.contains("scientific=1.23e3/1.23E3"), "{output}");
    assert!(output.contains("repeat=ff/é/ff"), "{output}");
}

/// Unrepresentable payloads never reach their encoder or corrupt later records.
#[test]
fn oversized_payload_never_encodes() {
    struct Huge(usize);
    impl zerolog::Encode for Huge {
        fn encoded_size(&self) -> usize {
            self.0
        }
        unsafe fn encode(&self, _: *mut u8, _: &mut usize) {
            panic!("oversized encode");
        }
        unsafe fn decode_and_format(_: *const u8, _: &mut usize, _: &mut Vec<u8>, _: &str) {
            panic!("oversized decode");
        }
    }
    // SAFETY: The encoder never returns, and therefore cannot write a mismatched layout.
    unsafe impl zerolog::EncodeFast for Huge {}
    let output = capture_output(Level::INF, || {
        zerolog::info!("overflow {} {}", Huge(usize::MAX), Huge(usize::MAX));
        zerolog::info!("too large {}", Huge(u32::MAX as usize));
        zerolog::info!("formatted too large {:>1}", Huge(u32::MAX as usize));
        zerolog::info!("healthy={}", 19u64);
    });
    assert!(!output.contains("overflow"), "{output}");
    assert!(!output.contains("too large"), "{output}");
    assert!(output.contains("healthy=19"), "{output}");
}

/// A real consumer names its only dependency `zl`; invalid invocations fail to compile.
#[test]
fn renamed_consumer_and_compile_failures() {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root =
        std::env::temp_dir().join(format!("zerolog-consumer-{}-{unique}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), format!(
        "[package]\nname='renamed-consumer'\nversion='0.0.0'\nedition='2024'\ndescription='application logger based on zerolog-rs'\n[workspace]\n[dependencies.zl]\npackage='zerolog-rs'\npath={:?}\n",
        env!("CARGO_MANIFEST_DIR"),
    )).unwrap();
    let check = |source: &str| {
        fs::write(root.join("src/main.rs"), source).unwrap();
        Command::new(env!("CARGO"))
            .args(["check", "--offline", "--quiet"])
            .env("CARGO_TARGET_DIR", root.join("target"))
            .current_dir(&root)
            .output()
            .unwrap()
    };
    let positive = check(
        r#"
        #[derive(Clone, Copy, zl::Encode)]
        struct BareWrapped<T>(T);
        impl<T: std::fmt::Display> std::fmt::Display for BareWrapped<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
        }
        #[derive(Clone, Copy, zl::Encode)]
        #[zerolog(crate = "zl")]
        struct Wrapped<T>(T);
        impl<T: std::fmt::Display> std::fmt::Display for Wrapped<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
        }
        fn emit<T: zl::EncodeFast>(value: T) { zl::info!("value={}", value); }
        fn main() {
            let text = String::from("borrowed");
            emit(text.as_str());
            emit(BareWrapped(77u64));
            emit(Wrapped(99u64));
            zl::trace!("{}", 1); zl::debug!("{}", 2); zl::warn!("{}", 3); zl::error!("{}", 4);
            zl::event!(zl::Level::INF, "structured"; "k" => 5u64, "opt" => Some("v"));
            zl::log!(@crate_path = zl; zl::Level::INF, "{0} {0}", &text);
            zl::set_header_pattern_static!(@crate_path = zl; "{level}| ").unwrap();
        }
    "#,
    );
    assert!(positive.status.success(), "{}", String::from_utf8_lossy(&positive.stderr));
    for (call, diagnostic) in [
        (r#"zl::info!("{:?}", 1);"#, "unsupported format trait"),
        (r#"zl::info!("{:p}", 1);"#, "unsupported format trait"),
        (r#"zl::info!("{} {0}", 1);"#, "cannot mix implicit and explicit"),
        (r#"zl::info!("{1}", 1);"#, "argument index out of range"),
        (r#"zl::info!("{0}", 1, 2);"#, "unused format argument"),
        (r#"zl::info!("{:99999999999999999999999999999999999}", 1);"#, "overflowing format"),
        (r#"zl::info!("{:.99999999999999999999999999999999999}", 1);"#, "overflowing format"),
    ] {
        let negative = check(&format!("fn main() {{ {call} }}"));
        assert!(!negative.status.success(), "accepted {call}");
        assert!(
            String::from_utf8_lossy(&negative.stderr).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&negative.stderr)
        );
    }
    let negative = check(
        r#"
        #[derive(Clone, Copy, zl::Encode)]
        #[zerolog(crate = "zl")]
        struct NotSend(&'static std::cell::Cell<u32>);
        impl std::fmt::Display for NotSend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.get().fmt(f) }
        }
        fn main() {}
    "#,
    );
    assert!(!negative.status.success());
    assert!(
        String::from_utf8_lossy(&negative.stderr)
            .contains("cannot be shared between threads safely"),
        "{}",
        String::from_utf8_lossy(&negative.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

/// Producer expressions and captures retain their surrounding lexical bindings.
#[test]
fn macro_locals_do_not_shadow_user_values() {
    fn __zerolog_size() -> u32 {
        23
    }
    let output = capture_output(Level::INF, || {
        let level = 42u32;
        let __zerolog_arg_0 = 99u32;
        zerolog::info!("{level} {} {}", __zerolog_arg_0, __zerolog_size());
        zerolog::event!(
            Level::INF,
            "order executed {}",
            7u32;
            "opt_some" => Some(42u64),
            "opt_none" => Option::<u64>::None,
            "res_ok" => Result::<&str, &str>::Ok("filled"),
            "bytes" => &[0xdeu8, 0xad, 0xbe, 0xef][..]
        );
    });
    assert!(output.contains("42 99 23"), "{output}");
    assert!(
        output.contains(
            "order executed 7 opt_some=Some(42) opt_none=None res_ok=Ok(filled) bytes=[0xde, 0xad, 0xbe, 0xef]"
        ),
        "{output}"
    );
}
