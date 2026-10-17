//! Compile `paveconel.rc` so the executable carries the application icon.
//!
//! Uses the SDK's `rc.exe` directly instead of pulling in a build-dependency: builds here go
//! through `build.bat`, which has already run `vcvars64.bat`, so `rc.exe` is on PATH. If it
//! is not (a bare `cargo build`), the build still succeeds - just without the icon resource.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/paveconel.ico");
    println!("cargo:rerun-if-changed=paveconel.rc");

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let res = PathBuf::from(out_dir).join("paveconel.res");

    match Command::new("rc.exe")
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("paveconel.rc")
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        _ => println!("cargo:warning=rc.exe unavailable; building without the app icon"),
    }
}
