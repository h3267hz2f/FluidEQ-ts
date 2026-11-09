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
            .define("FEQ_CORE_VERSION", "\"0.1.0-identity\"")
            .include(&include_dir)
            .include(&src_dir)
            .include(&vendor_dir)
            .opt_level(3)
            .warnings(false);

        let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if target_env == "msvc" {
            build
                .flag("/EHsc")
                .flag("/GR-")
                .flag("/fp:precise")
                .flag("/Brepro")
                .flag("/utf-8");
        } else {
            build
                .flag_if_supported("-fno-rtti")
                .flag_if_supported("-ffp-contract=off");
        }

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

        let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target_env != "msvc" {
            if target_os == "windows" {
                println!("cargo:rustc-link-lib=pthread");
                println!("cargo:rustc-link-lib=kernel32");
            } else {
                println!("cargo:rustc-link-lib=dl");
                println!("cargo:rustc-link-lib=pthread");
            }
        }
    }

    let _ = fs::metadata(&include_dir);
}
