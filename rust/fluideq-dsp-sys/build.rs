/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../.."));

    let dsp_core_dir = repo_root.join("native").join("dsp-core");
    let include_dir = dsp_core_dir.join("include");
    let src_dir = dsp_core_dir.join("src");
    let vendor_dir = repo_root
        .join("native")
        .join("dsp-host")
        .join("src")
        .join("vendor");

    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed={}", src_dir.display());
    println!("cargo:rerun-if-changed={}", vendor_dir.display());

    #[cfg(feature = "link-cpp-core")]
    {
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std("c++20")
            .include(&include_dir)
            .include(&src_dir)
            .include(&vendor_dir)
            .opt_level(3)
            .warnings(false);

        if let Ok(entries) = fs::read_dir(&src_dir) {
            let mut cpp_files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("cpp"))
                .collect();
            cpp_files.sort();
            for file in cpp_files {
                build.file(file);
            }
        }

        build.compile("fluideq_dsp_core");
    }

    let _ = fs::metadata(&include_dir);
}
