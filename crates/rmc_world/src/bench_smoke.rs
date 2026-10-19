//! Wiring check for the criterion benches of SAD §13 and §14.
//!
//! Each bench lives in the crate whose code it measures, as a
//! `harness = false` target that `cargo bench` runs and that
//! `cargo test --all-targets` and `cargo clippy --all-targets` build (and
//! criterion runs once in its test mode). Their results are recorded in
//! `docs/perf.md`.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// `(crate, bench)` for every bench SAD §14 names.
    const BENCHES: [(&str, &str); 4] = [
        ("rmc_world", "palette_set_get"),
        ("rmc_worldgen", "gen_world"),
        ("rmc_host", "host_tick_100"),
        ("rmc_client", "mesh_chunk"),
    ];

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
            .replace("\r\n", "\n")
    }

    /// The `[[bench]]` tables of a manifest, each as its lines without the
    /// header.
    fn bench_tables(manifest: &str) -> Vec<Vec<String>> {
        let mut tables = Vec::new();
        let mut current: Option<Vec<String>> = None;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                tables.extend(current.take());
                if line == "[[bench]]" {
                    current = Some(Vec::new());
                }
            } else if let Some(t) = current.as_mut()
                && !line.is_empty()
                && !line.starts_with('#')
            {
                t.push(line.replace(' ', ""));
            }
        }
        tables.extend(current);
        tables
    }

    #[test]
    fn benches_compile() {
        let root = root();
        let perf = read(&root.join("docs/perf.md"));
        for (krate, name) in BENCHES {
            let dir = root.join("crates").join(krate);
            let manifest = read(&dir.join("Cargo.toml"));
            assert!(
                manifest
                    .replace(' ', "")
                    .contains("criterion.workspace=true"),
                "{krate} has no criterion dev-dependency"
            );
            let table = bench_tables(&manifest)
                .into_iter()
                .find(|t| t.contains(&format!("name=\"{name}\"")))
                .unwrap_or_else(|| panic!("{krate}/Cargo.toml declares no [[bench]] {name}"));
            assert!(
                table.contains(&"harness=false".to_string()),
                "bench {name} must set harness = false for criterion"
            );

            let source = read(&dir.join("benches").join(format!("{name}.rs")));
            assert!(
                source.contains("criterion_main!"),
                "benches/{name}.rs has no criterion_main!"
            );
            assert!(
                source.contains(&format!("\"{name}\"")),
                "benches/{name}.rs registers no benchmark named {name:?}"
            );

            let row = perf
                .lines()
                .find(|l| l.starts_with('|') && l.contains(&format!("`{name}`")))
                .unwrap_or_else(|| panic!("docs/perf.md has no table row for `{name}`"));
            assert!(
                row.chars().any(|c| c.is_ascii_digit()),
                "docs/perf.md row for `{name}` records no measurement: {row}"
            );
        }
    }
}
