use std::sync::{Arc, Mutex};

use nils_term::progress::{Progress, ProgressDrawTarget, ProgressEnabled, ProgressOptions};

fn lock_buffer(buffer: &Arc<Mutex<Vec<u8>>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    match buffer.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn read_output(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&lock_buffer(buffer)).to_string()
}

fn normalize(s: &str) -> String {
    // Remove carriage returns and a basic subset of ANSI escape sequences.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            i += 1;
            continue;
        }

        if bytes[i] == 0x1b {
            // Skip CSI sequences: ESC [ ... <final-byte>
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                i += 2;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if b.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
        }

        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[test]
fn disabled_mode_produces_no_output() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let opts = ProgressOptions::default()
        .with_enabled(ProgressEnabled::Off)
        .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
        .with_width(Some(60));

    let p = Progress::new(3, opts);
    p.inc(1);
    p.finish();

    assert!(read_output(&buffer).is_empty());
}

#[test]
fn writer_target_tty_contract_distinguishes_auto_and_off() {
    let auto_buffer = Arc::new(Mutex::new(Vec::new()));
    let auto = Progress::new(
        2,
        ProgressOptions::default()
            .with_enabled(ProgressEnabled::Auto)
            .with_draw_target(ProgressDrawTarget::to_writer(auto_buffer.clone()))
            .with_width(Some(60))
            .with_prefix("tty "),
    );
    auto.inc(1);
    auto.finish();

    let auto_out = normalize(&read_output(&auto_buffer));
    assert!(auto_out.contains("1/2"), "output was: {auto_out:?}");
    assert!(auto_out.contains("tty"), "output was: {auto_out:?}");

    let off_buffer = Arc::new(Mutex::new(Vec::new()));
    let off = Progress::new(
        2,
        ProgressOptions::default()
            .with_enabled(ProgressEnabled::Off)
            .with_draw_target(ProgressDrawTarget::to_writer(off_buffer.clone()))
            .with_width(Some(60))
            .with_prefix("tty "),
    );
    off.inc(1);
    off.finish();

    assert!(
        read_output(&off_buffer).is_empty(),
        "off mode should stay silent"
    );
}

#[test]
fn determinate_renders_and_finishes() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let opts = ProgressOptions::default()
        .with_enabled(ProgressEnabled::Auto)
        .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
        .with_width(Some(60))
        .with_prefix("demo ");

    let p = Progress::new(3, opts);
    p.set_message("one");
    p.inc(1);
    p.finish();

    let out = normalize(&read_output(&buffer));
    assert!(out.contains("1/3"), "output was: {out:?}");
    assert!(out.contains("demo"), "output was: {out:?}");
    assert!(out.contains("one"), "output was: {out:?}");
}

#[test]
fn spinner_renders_tick_and_finishes() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let opts = ProgressOptions::default()
        .with_enabled(ProgressEnabled::Auto)
        .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
        .with_width(Some(60))
        .with_prefix("spin ");

    let p = Progress::spinner(opts);
    p.set_message("loading");
    p.tick();
    p.finish_and_clear();

    let out = normalize(&read_output(&buffer));
    assert!(out.contains("spin"), "output was: {out:?}");
    assert!(out.contains("loading"), "output was: {out:?}");
}

#[test]
fn fixed_width_affects_rendered_bar_length() {
    fn render_with_width(width: u16) -> String {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let opts = ProgressOptions::default()
            .with_enabled(ProgressEnabled::Auto)
            .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
            .with_width(Some(width));

        let p = Progress::new(10, opts);
        p.inc(1);
        p.finish();

        normalize(&read_output(&buffer))
    }

    let small = render_with_width(20);
    let large = render_with_width(80);

    let small_prefix_len = small.find("1/10").expect("small contains pos/len");
    let large_prefix_len = large.find("1/10").expect("large contains pos/len");

    assert!(
        large_prefix_len > small_prefix_len,
        "expected larger width to render longer bar: small_len={small_prefix_len} large_len={large_prefix_len}"
    );
}

#[test]
fn suspend_does_not_panic() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let opts = ProgressOptions::default()
        .with_enabled(ProgressEnabled::Auto)
        .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
        .with_width(Some(60));

    let p = Progress::spinner(opts);
    p.tick();
    p.suspend(|| {});
    p.finish();
}

#[test]
fn writer_target_recovers_after_buffer_poisoning() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let poison_target = buffer.clone();

    let _ = std::thread::spawn(move || {
        let _guard = poison_target.lock().expect("buffer lock");
        panic!("poison writer buffer");
    })
    .join();

    assert!(buffer.lock().is_err(), "expected poisoned mutex");

    let opts = ProgressOptions::default()
        .with_enabled(ProgressEnabled::On)
        .with_draw_target(ProgressDrawTarget::to_writer(buffer.clone()))
        .with_width(Some(60))
        .with_prefix("poison ");

    let p = Progress::new(2, opts);
    p.inc(1);
    p.finish();

    let out = normalize(&read_output(&buffer));
    assert!(out.contains("1/2"), "output was: {out:?}");
    assert!(out.contains("poison"), "output was: {out:?}");
}
