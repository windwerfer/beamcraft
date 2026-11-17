#![forbid(unsafe_code)]
//! `cargo xtask ci [--no-loopback]`: the local gate from AGENTS.md section 3 (fmt, clippy, tests,
//! crate-layer check, loopback). `cargo xtask layers` runs only the layer check.
//! `cargo audit` and `cargo deny` are separate installs and stay manual (see testing.md).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Allowed `mirrorwire-*` path dependencies per crate (architecture.md section 1).
/// Lower layers never depend on higher ones.
const LAYERS: &[(&str, &[&str])] = &[
    ("mirrorwire-proto", &[]),
    ("mirrorwire-film", &[]),
    ("mirrorwire-media", &["mirrorwire-proto", "mirrorwire-film"]),
    ("mirrorwire-net", &["mirrorwire-proto"]),
    (
        "mirrorwire-app",
        &[
            "mirrorwire-proto",
            "mirrorwire-media",
            "mirrorwire-net",
            "mirrorwire-film",
        ],
    ),
    (
        "mirrorwire-android",
        &["mirrorwire-proto", "mirrorwire-net", "mirrorwire-film"],
    ),
];

/// Non-workspace crates proto may use (architecture.md: no I/O, async or FFmpeg).
const PROTO_EXTERNAL: &[&str] = &[
    "bytes",
    "serde",
    "serde_json",
    "anyhow",
    "reed-solomon-simd",
    "tracing",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("ci") => ci(!args.iter().any(|a| a == "--no-loopback")),
        Some("layers") => layers(&root()),
        _ => Err("usage: cargo xtask <ci [--no-loopback] | layers>".to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    // xtask/ sits directly below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn run(name: &str, program: &str, args: &[&str]) -> Result<(), String> {
    eprintln!("\n==> {name}: {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(root())
        .status()
        .map_err(|e| format!("{name}: cannot start {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} failed ({status})"))
    }
}

fn ci(loopback: bool) -> Result<(), String> {
    run("fmt", "cargo", &["fmt", "--all", "--", "--check"])?;
    layers(&root())?;
    run(
        "clippy",
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run("test", "cargo", &["test", "--workspace"])?;
    if loopback {
        run(
            "loopback",
            "scripts/loopback-test.sh",
            &["10", "--encoder", "x264"],
        )?;
    }
    eprintln!("\nxtask ci: all gates passed (cargo audit / cargo deny run separately)");
    Ok(())
}

/// Dependency names declared in a manifest (`name = ...` / `name.workspace = true` lines inside
/// any `dependencies` table), without a TOML parser: this crate has no dependencies.
fn dependencies(manifest: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut out = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line.trim_end_matches(']').ends_with("dependencies");
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let name = key.trim().split('.').next().unwrap_or("").trim();
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn layers(root: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    for (krate, allowed) in LAYERS {
        let path = root.join("crates").join(krate).join("Cargo.toml");
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let deps = dependencies(&text);
        for dep in deps.iter().filter(|d| d.starts_with("mirrorwire-")) {
            if !allowed.contains(&dep.as_str()) {
                errors.push(format!(
                    "{krate} must not depend on {dep} (architecture.md layers)"
                ));
            }
        }
        if *krate == "mirrorwire-proto" {
            for dep in deps
                .iter()
                .filter(|d| !PROTO_EXTERNAL.contains(&d.as_str()))
            {
                errors.push(format!(
                    "{krate} must not depend on {dep} (allowed: {PROTO_EXTERNAL:?})"
                ));
            }
        }
    }
    // A new crate must be added to LAYERS.
    if let Ok(entries) = fs::read_dir(root.join("crates")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !LAYERS.iter().any(|(k, _)| *k == name) {
                errors.push(format!("crates/{name} is not listed in xtask LAYERS"));
            }
        }
    }
    if errors.is_empty() {
        eprintln!("layers: ok");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dependency_names() {
        let m = "[package]\nname = \"x\"\n[dependencies]\nbytes.workspace = true\n# c\nserde = { version = \"1\" }\n[dev-dependencies]\nfoo = \"1\"\n[features]\nbar = []\n";
        assert_eq!(dependencies(m), vec!["bytes", "serde", "foo"]);
    }
}
