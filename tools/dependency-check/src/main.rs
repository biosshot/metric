use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const CRATES: &[(&str, &str, &[&str])] = &[
    ("faultkeep-domain", "crates/domain", &[]),
    ("faultkeep-ports", "crates/ports", &["faultkeep-domain"]),
    (
        "faultkeep-sentry-protocol",
        "crates/sentry-protocol",
        &["faultkeep-domain"],
    ),
    (
        "faultkeep-application",
        "crates/application",
        &["faultkeep-domain", "faultkeep-ports"],
    ),
    (
        "faultkeep-mongo",
        "crates/mongo",
        &["faultkeep-domain", "faultkeep-ports"],
    ),
    (
        "faultkeep-blob",
        "crates/blob",
        &["faultkeep-domain", "faultkeep-ports"],
    ),
    (
        "faultkeep-symbolication",
        "crates/symbolication",
        &["faultkeep-domain", "faultkeep-ports"],
    ),
    (
        "faultkeep-server",
        "crates/server",
        &[
            "faultkeep-application",
            "faultkeep-blob",
            "faultkeep-domain",
            "faultkeep-mongo",
            "faultkeep-ports",
            "faultkeep-sentry-protocol",
            "faultkeep-symbolication",
        ],
    ),
    (
        "faultkeep-testkit",
        "crates/testkit",
        &[
            "faultkeep-application",
            "faultkeep-domain",
            "faultkeep-ports",
        ],
    ),
];

fn main() -> ExitCode {
    match check() {
        Ok(()) => {
            println!("dependency graph satisfies ADR-0034");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dependency graph violation: {error}");
            ExitCode::FAILURE
        }
    }
}

fn check() -> Result<(), String> {
    let root = workspace_root()?;
    let local_names = CRATES
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<BTreeSet<_>>();
    let mut graph = BTreeMap::<&str, BTreeSet<String>>::new();

    for (name, directory, allowed) in CRATES {
        let manifest = root.join(directory).join("Cargo.toml");
        let declared = local_dependencies(&manifest, &local_names)?;
        let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
        for dependency in &declared {
            if !allowed.contains(dependency.as_str()) {
                return Err(format!("{name} imports forbidden local crate {dependency}"));
            }
        }
        graph.insert(name, declared);
    }

    detect_cycle(&graph)
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot find workspace root".to_owned())
}

fn local_dependencies(
    manifest: &Path,
    local_names: &BTreeSet<&str>,
) -> Result<BTreeSet<String>, String> {
    let content = fs::read_to_string(manifest)
        .map_err(|error| format!("cannot read {}: {error}", manifest.display()))?;
    let mut in_dependencies = false;
    let mut dependencies = BTreeSet::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let package = key.trim().replace('_', "-");
        if local_names.contains(package.as_str()) {
            dependencies.insert(package);
        }
    }
    Ok(dependencies)
}

fn detect_cycle(graph: &BTreeMap<&str, BTreeSet<String>>) -> Result<(), String> {
    fn visit<'a>(
        node: &'a str,
        graph: &'a BTreeMap<&str, BTreeSet<String>>,
        active: &mut BTreeSet<&'a str>,
        complete: &mut BTreeSet<&'a str>,
    ) -> Result<(), String> {
        if complete.contains(node) {
            return Ok(());
        }
        if !active.insert(node) {
            return Err(format!("cycle contains {node}"));
        }
        if let Some(dependencies) = graph.get(node) {
            for dependency in dependencies {
                visit(dependency, graph, active, complete)?;
            }
        }
        active.remove(node);
        complete.insert(node);
        Ok(())
    }

    let mut active = BTreeSet::new();
    let mut complete = BTreeSet::new();
    for node in graph.keys() {
        visit(node, graph, &mut active, &mut complete)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_workspace_respects_dependency_direction() {
        check().unwrap();
    }

    #[test]
    fn cycle_detection_rejects_a_cycle() {
        let graph = BTreeMap::from([
            ("a", BTreeSet::from(["b".to_owned()])),
            ("b", BTreeSet::from(["a".to_owned()])),
        ]);
        assert!(detect_cycle(&graph).is_err());
    }
}
