//! Native CI probe: build normally so the macOS Keychain backend is exercised.
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use codexbar::secure_file::{self, SecureFileStatus};
    if cfg!(feature = "test-secure-storage") {
        return Err("The native Keychain probe cannot use test-secure-storage".into());
    }
    const SAMPLE: &str = "CodexBar native secure storage probe";
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("Usage: secure_file_probe write|read <temporary-file>".into());
    }
    let path = std::path::Path::new(&args[1]);
    match args[0].as_str() {
        "write" => {
            secure_file::write_string(path, SAMPLE)?;
            assert!(
                matches!(secure_file::status(path), SecureFileStatus::Protected(tag) if tag == "macos-keychain-aes256-gcm")
            );
            assert!(!std::fs::read_to_string(path)?.contains(SAMPLE));
        }
        "read" => assert_eq!(secure_file::read_string(path)?, SAMPLE),
        _ => return Err("Unknown probe operation".into()),
    }
    println!("Native secure storage probe passed");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Run the native Keychain integration probe on macOS");
}
