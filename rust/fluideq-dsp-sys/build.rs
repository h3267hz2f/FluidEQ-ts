/*
<FluidEQ: System-wide parametric audio equalizer interface>
Copyright (C) <2026>  <Ivan Carmenates Garcia>
SPDX-License-Identifier: GPL-3.0-or-later
*/

//! Build bridge for the FluidEQ C++ DSP core.
//!
//! Two generated artefacts come from the one parameter table in
//! `native/parameters/nativeParameters.ts`:
//!
//! * `fluideq/parameters.h` in `OUT_DIR`, which the C++ core includes, and
//! * `parameters.rs` in `OUT_DIR`, which `fluideq-dsp-sys` includes.
//!
//! Both are generated from the same source on every build, so the sparse ids the
//! host speaks and the sparse ids the engine looks up cannot drift apart. The
//! TypeScript table is the authority; nothing here carries a second copy of it.
//!
//! When the `link-cpp-core` feature is on, the C++ translation units are also
//! compiled and archived into `libfluideq_dsp_core`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

struct NativeParameter {
    id: u32,
    path: String,
    structural: bool,
}

/// `exciter.bands[].freqHz` becomes `EXCITER_BANDS_FREQ_HZ`.
///
/// Mirrors the TypeScript writer exactly: a boundary exists only between a
/// lower-case letter or digit and an upper-case letter, so `gainDb` is
/// `GAIN_DB`, not `G_A_I_N_DB`.
fn macro_name(parameter_path: &str) -> String {
    parameter_path
        .replace("[]", "")
        .split('.')
        .map(|segment| {
            let characters: Vec<char> = segment.chars().collect();
            let mut out = String::with_capacity(segment.len() + 4);
            for (index, ch) in characters.iter().enumerate() {
                let previous = index.checked_sub(1).map(|at| characters[at]);
                if ch.is_ascii_uppercase()
                    && previous
                        .is_some_and(|value| value.is_ascii_lowercase() || value.is_ascii_digit())
                {
                    out.push('_');
                }
                out.push(ch.to_ascii_uppercase());
            }
            out
        })
        .collect::<Vec<_>>()
        .join("_")
}

fn read_string_literal(object: &str, key: &str) -> Option<String> {
    let at = object.find(key)?;
    let rest = &object[at + key.len()..];
    let rest = rest.trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn parse_native_parameters(text: &str) -> Vec<NativeParameter> {
    let marker = "NATIVE_DSP_PARAMETERS = [";
    let start = text
        .find(marker)
        .map(|at| at + marker.len())
        .unwrap_or_else(|| panic!("nativeParameters.ts: `{marker}` not found"));
    let end = text[start..]
        .find("] as const")
        .map(|at| start + at)
        .unwrap_or(text.len());
    let body = &text[start..end];

    let mut parameters = Vec::new();
    let mut depth = 0usize;
    let mut object_start = 0usize;
    for (at, ch) in body.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    object_start = at;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let object = &body[object_start..=at];
                    let id = object
                        .find("id:")
                        .and_then(|found| {
                            object[found + 3..]
                                .trim_start()
                                .split(|c: char| !c.is_ascii_digit())
                                .next()
                                .and_then(|digits| digits.parse::<u32>().ok())
                        })
                        .unwrap_or_else(|| panic!("nativeParameters.ts: parameter without an id"));
                    let path = read_string_literal(object, "path:").unwrap_or_else(|| {
                        panic!("nativeParameters.ts: parameter {id} without a path")
                    });
                    parameters.push(NativeParameter {
                        id,
                        path,
                        structural: object.contains("structural: true"),
                    });
                }
            }
            _ => {}
        }
    }

    if parameters.is_empty() {
        panic!("nativeParameters.ts: no parameters parsed");
    }
    parameters
}

fn parse_schema_version(text: &str) -> u32 {
    let marker = "NATIVE_DSP_PARAMETER_SCHEMA_VERSION = ";
    let start = text
        .find(marker)
        .map(|at| at + marker.len())
        .unwrap_or_else(|| panic!("nativeParameters.ts: schema version not found"));
    text[start..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or_else(|| panic!("nativeParameters.ts: schema version is not a number"))
}

fn write_cpp_header(out: &Path, parameters: &[NativeParameter], schema_version: u32) {
    let mut lines: Vec<String> = vec![
        "/*".to_string(),
        "<FluidEQ: System-wide parametric audio equalizer interface>".to_string(),
        "Copyright (C) <2026>  <Ivan Carmenates Garcia>".to_string(),
        "SPDX-License-Identifier: GPL-3.0-or-later".to_string(),
        "*/".to_string(),
        String::new(),
        "/*".to_string(),
        " * GENERATED FILE. Do not edit.".to_string(),
        " *".to_string(),
        " * Source: native/parameters/nativeParameters.ts".to_string(),
        " * Writer: rust/fluideq-dsp-sys/build.rs".to_string(),
        " */".to_string(),
        "#ifndef FLUIDEQ_PARAMETERS_H".to_string(),
        "#define FLUIDEQ_PARAMETERS_H".to_string(),
        String::new(),
        "#include <stdint.h>".to_string(),
        String::new(),
        format!("#define FEQ_PARAMETER_SCHEMA_VERSION {schema_version}"),
        format!("#define FEQ_PARAMETER_COUNT {}", parameters.len()),
        String::new(),
        "enum FeqParameterId {".to_string(),
    ];
    for parameter in parameters {
        lines.push(format!(
            "  FEQ_PARAM_{} = {},",
            macro_name(&parameter.path),
            parameter.id
        ));
    }
    lines.push("};".to_string());
    lines.push(String::new());
    lines.push("static const uint32_t FEQ_PARAMETER_IDS[FEQ_PARAMETER_COUNT] = {".to_string());
    for parameter in parameters {
        lines.push(format!("  {}u, /* {} */", parameter.id, parameter.path));
    }
    lines.push("};".to_string());
    lines.push(String::new());
    lines
        .push("static const uint8_t FEQ_PARAMETER_STRUCTURAL[FEQ_PARAMETER_COUNT] = {".to_string());
    for parameter in parameters {
        lines.push(format!(
            "  {}, /* {} */",
            if parameter.structural { 1 } else { 0 },
            parameter.path
        ));
    }
    lines.push("};".to_string());
    lines.push(String::new());
    lines.push("static inline int feq_parameter_slot(uint32_t id) {".to_string());
    lines.push("  for (int slot = 0; slot < FEQ_PARAMETER_COUNT; ++slot) {".to_string());
    lines.push("    if (FEQ_PARAMETER_IDS[slot] == id) {".to_string());
    lines.push("      return slot;".to_string());
    lines.push("    }".to_string());
    lines.push("  }".to_string());
    lines.push("  return -1;".to_string());
    lines.push("}".to_string());
    lines.push(String::new());
    lines.push("#endif /* FLUIDEQ_PARAMETERS_H */".to_string());
    lines.push(String::new());

    let target = out.join("fluideq");
    fs::create_dir_all(&target).expect("create generated include directory");
    fs::write(target.join("parameters.h"), lines.join("\n")).expect("write parameters.h");
}

fn write_rust_module(out: &Path, parameters: &[NativeParameter], schema_version: u32) {
    let mut lines = vec![
        "/// Schema version of the core parameter block (`FEQ_PARAMETER_SCHEMA_VERSION`)."
            .to_string(),
        format!("pub const FEQ_PARAMETER_SCHEMA_VERSION: u32 = {schema_version};"),
        "/// The engine's flat parameter count (`FEQ_PARAMETER_COUNT`).".to_string(),
        format!(
            "pub const FEQ_PARAMETER_COUNT: usize = {};",
            parameters.len()
        ),
        "/// Sparse ids in table order; the index is the dense slot.".to_string(),
        "pub const FEQ_PARAMETER_IDS: [u32; FEQ_PARAMETER_COUNT] = [".to_string(),
    ];
    for parameter in parameters {
        lines.push(format!("    {}, // {}", parameter.id, parameter.path));
    }
    lines.push("];".to_string());
    lines.push(
        "/// Whether applying this parameter needs preparation off the audio thread.".to_string(),
    );
    lines.push("pub const FEQ_PARAMETER_STRUCTURAL: [bool; FEQ_PARAMETER_COUNT] = [".to_string());
    for parameter in parameters {
        lines.push(format!(
            "    {}, // {}",
            parameter.structural, parameter.path
        ));
    }
    lines.push("];".to_string());
    lines.push(String::new());
    lines.push("/// Sparse id to dense slot. `None` is a refusal, never slot 0.".to_string());
    lines.push("pub fn feq_parameter_slot(id: u32) -> Option<usize> {".to_string());
    lines.push("    FEQ_PARAMETER_IDS.iter().position(|candidate| *candidate == id)".to_string());
    lines.push("}".to_string());
    lines.push(String::new());

    fs::write(out.join("parameters.rs"), lines.join("\n")).expect("write parameters.rs");
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../.."));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    let dsp_core_dir = repo_root.join("native").join("dsp-core");
    let include_dir = dsp_core_dir.join("include");
    let src_dir = dsp_core_dir.join("src");
    let vendor_dir = repo_root
        .join("native")
        .join("dsp-host")
        .join("src")
        .join("vendor");
    let parameter_source = repo_root
        .join("native")
        .join("parameters")
        .join("nativeParameters.ts");

    println!("cargo:rerun-if-changed={}", include_dir.display());
    println!("cargo:rerun-if-changed={}", src_dir.display());
    println!("cargo:rerun-if-changed={}", vendor_dir.display());
    println!("cargo:rerun-if-changed={}", parameter_source.display());

    let parameter_text = fs::read_to_string(&parameter_source).unwrap_or_else(|error| {
        panic!(
            "cannot read the parameter table {}: {error}",
            parameter_source.display()
        )
    });
    let parameters = parse_native_parameters(&parameter_text);
    let schema_version = parse_schema_version(&parameter_text);
    write_cpp_header(&out_dir, &parameters, schema_version);
    write_rust_module(&out_dir, &parameters, schema_version);

    #[cfg(feature = "link-cpp-core")]
    {
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std("c++20")
            .define("FEQ_CORE_VERSION", "\"0.1.0-identity\"")
            .include(&out_dir)
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
