//! Tamka: dictation for Apple silicon Macs. Press ⌃V, speak, press ⌃V again, and
//! the text is pasted where you're typing. Speech runs on the Neural Engine
//! (Parakeet via FluidAudio) in a helper process; this app is the menu-bar shell.

mod app;
mod audio;
mod engine;
mod hotkey;
mod hud;
mod keys;
mod models;
mod paste;
mod permissions;
mod settings;
mod settings_window;
mod text;
mod ui;
mod util;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("Tamka is built for Apple silicon Macs only (aarch64-apple-darwin).");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // `Tamka --transcribe a.wav b.wav`: run files through the engine and the
        // text cleanup, print the results. For testing without a microphone.
        Some("--transcribe") => transcribe_files(&args[1..]),
        // `Tamka --login-item on|off|status`
        Some("--login-item") => {
            let result = match args.get(1).map(String::as_str) {
                Some("on") => app::set_login_item(true),
                Some("off") => app::set_login_item(false),
                _ => Ok(()),
            };
            match result {
                Ok(()) => println!("Launch at Login: {}", if app::login_item_enabled() { "on" } else { "off" }),
                Err(e) => {
                    eprintln!("Couldn't change Launch at Login: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => app::run(),
    }
}

fn transcribe_files(files: &[String]) {
    let s = settings::get();
    let opts = text::Options { remove_fillers: s.remove_fillers, collapse_repeats: s.collapse_repeats };
    let mut failed = false;
    for f in files {
        let t0 = std::time::Instant::now();
        match engine::transcribe(std::path::Path::new(f)) {
            Ok(raw) => {
                println!("== {f} ({} ms)", t0.elapsed().as_millis());
                println!("raw:   {raw}");
                println!("final: {}", text::process(&raw, &s.find_replace, &opts));
            }
            Err(e) => {
                eprintln!("== {f}: {e}");
                failed = true;
            }
        }
    }
    engine::shutdown();
    if failed {
        std::process::exit(1);
    }
}
