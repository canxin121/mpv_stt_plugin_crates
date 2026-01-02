use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    println!("cargo:rerun-if-env-changed=MPV_PREFIX");

    // HTTP path uses audiopus; no extra C shim needed. Ensure libopus search path is known.
    if let Ok(root) = env::var("DEP_AUDIOPUS_SYS_ROOT") {
        let lib_path = Path::new(&root).join("lib");
        let lib64_path = Path::new(&root).join("lib64");

        // audiopus_sys always emits -L{root}/lib; when only lib64 exists, provide a shim.
        if !lib_path.exists() && lib64_path.exists() {
            #[cfg(unix)]
            {
                let _ = std::os::unix::fs::symlink(&lib64_path, &lib_path);
            }
            #[cfg(windows)]
            {
                let _ = std::os::windows::fs::symlink_dir(&lib64_path, &lib_path);
            }
        }

        let mut found = false;
        for sub in ["lib64", "lib"] {
            let path = format!("{root}/{sub}");
            if std::path::Path::new(&path).exists() {
                println!("cargo:rustc-link-search=native={path}");
                found = true;
            }
        }
        if found {
            // Link statically to avoid relying on system-wide libopus.
            println!("cargo:rustc-link-lib=static=opus");
        }
    }

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("android") {
        return;
    }

    // On Android we ship libmpv from the prebuilt prefix.
    // If the prefix is missing, fail fast instead of producing a broken .so.
    let mpv_prefix = env::var("MPV_PREFIX").unwrap_or_default();
    let mpv_lib_dir = env::var("MPV_LIB_DIR").unwrap_or_else(|_| format!("{mpv_prefix}/lib"));
    println!("cargo:rustc-link-search=native={mpv_lib_dir}");
    println!("cargo:rustc-link-lib=mpv");
}
