//! meta-rust library
//!
//! Runs any cargo command across the Rust repos of a meta workspace.

pub use meta_plugin_protocol::{CommandResult, PlannedCommand, PluginRequestOptions};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Plan `cargo <args>` in every selected repo that has a `Cargo.toml`.
///
/// `projects` is the repo list from meta_cli (it includes the meta root);
/// when it is empty the plan runs in `cwd` only. Include/exclude filters are
/// applied here, the same way loop_lib applies them, so that workspace
/// members are only skipped when their workspace root is really selected.
pub fn execute_command(
    args: &[String],
    options: &PluginRequestOptions,
    projects: &[String],
    cwd: &Path,
) -> CommandResult {
    // `meta cargo` and `meta cargo <cmd> --help` show this plugin's help
    // instead of running cargo once per repo. Args after `--` belong to cargo.
    let mut before_separator = args.iter().take_while(|a| *a != "--");
    if args.is_empty() || before_separator.any(|a| a == "--help" || a == "-h") {
        return CommandResult::ShowHelp(None);
    }

    let cmd = match shlex::try_join(std::iter::once("cargo").chain(args.iter().map(String::as_str)))
    {
        Ok(cmd) => cmd,
        Err(e) => return CommandResult::Error(format!("Cannot quote cargo arguments: {e}")),
    };

    let dirs: Vec<String> = if projects.is_empty() {
        vec![".".to_string()]
    } else {
        projects.to_vec()
    };
    // Notes name repos by their path from the meta root, which meta_cli lists first.
    let root = cwd.join(dirs.first().map_or(".", String::as_str));
    let dirs = filter_dirs(dirs, options);
    let (mut rust_dirs, other_dirs): (Vec<String>, Vec<String>) = dirs
        .into_iter()
        .partition(|dir| cwd.join(dir).join("Cargo.toml").is_file());
    let workspaces = workspace_members(&rust_dirs, cwd);
    rust_dirs.retain(|dir| !workspaces.iter().any(|(_, members)| members.contains(dir)));

    if !options.silent {
        note(&other_dirs, "no Cargo.toml", &root);
        for (workspace, members) in &workspaces {
            let reason = format!(
                "cargo runs on them from the workspace at {}",
                display(workspace, &root)
            );
            note(members, &reason, &root);
        }
    }

    if rust_dirs.is_empty() {
        return CommandResult::Message("No Rust projects found (no Cargo.toml files)".to_string());
    }

    let commands = rust_dirs
        .into_iter()
        .map(|dir| PlannedCommand {
            dir,
            cmd: cmd.clone(),
            env: None,
        })
        .collect();
    CommandResult::Plan(commands, Some(options.parallel))
}

/// Keep the dirs loop_lib will run in (substring match, like loop_lib).
fn filter_dirs(mut dirs: Vec<String>, options: &PluginRequestOptions) -> Vec<String> {
    if let Some(includes) = options.include_filters.as_ref().filter(|f| !f.is_empty()) {
        dirs.retain(|dir| includes.iter().any(|f| dir.contains(f.as_str())));
    }
    if let Some(excludes) = &options.exclude_filters {
        dirs.retain(|dir| {
            !excludes
                .iter()
                .any(|f| dir.contains(f.trim_end_matches('/')))
        });
    }
    dirs
}

/// Each dir in `rust_dirs` that is a workspace root, with the other dirs in
/// `rust_dirs` whose package it builds by default. Cargo in the root already
/// runs on those members.
fn workspace_members(rust_dirs: &[String], cwd: &Path) -> Vec<(String, Vec<String>)> {
    let manifest = |dir: &str| cwd.join(dir).join("Cargo.toml").canonicalize().ok();
    let mut workspaces = Vec::new();
    for root in rust_dirs {
        let root_path = cwd.join(root);
        let nested = || {
            rust_dirs
                .iter()
                .filter(|dir| *dir != root && cwd.join(dir).starts_with(&root_path))
        };
        if nested().next().is_none() {
            continue;
        }
        let manifests = default_member_manifests(&root_path);
        let members: Vec<String> = nested()
            .filter(|dir| manifest(dir).is_some_and(|m| manifests.contains(&m)))
            .cloned()
            .collect();
        if !members.is_empty() {
            workspaces.push((root.clone(), members));
        }
    }
    workspaces
}

/// Manifests of the packages `cargo` in `root` acts on by default, or none if
/// `root` is not a workspace root or some members are not default members.
fn default_member_manifests(root: &Path) -> Vec<PathBuf> {
    let Ok(output) = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    let Ok(meta) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Vec::new();
    };
    let is_root = meta["workspace_root"]
        .as_str()
        .and_then(|r| Path::new(r).canonicalize().ok())
        .zip(root.canonicalize().ok())
        .is_some_and(|(a, b)| a == b);
    if !is_root || meta["workspace_members"] != meta["workspace_default_members"] {
        return Vec::new();
    }
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["manifest_path"].as_str())
        .filter_map(|m| Path::new(m).canonicalize().ok())
        .collect()
}

fn note(dirs: &[String], reason: &str, root: &Path) {
    if dirs.is_empty() {
        return;
    }
    let names: Vec<String> = dirs.iter().map(|dir| display(dir, root)).collect();
    eprintln!("meta-rust: skipping {} ({reason})", names.join(", "));
}

fn display(dir: &str, root: &Path) -> String {
    match root.join(dir).strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel.display().to_string(),
        Err(_) => dir.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: PathBuf, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn plan(args: &[&str], projects: &[&str], cwd: &Path) -> Vec<PlannedCommand> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let projects: Vec<String> = projects.iter().map(|p| p.to_string()).collect();
        let options = PluginRequestOptions::default();
        match execute_command(&args, &options, &projects, cwd) {
            CommandResult::Plan(commands, _) => commands,
            _ => panic!("Expected Plan result"),
        }
    }

    #[test]
    fn any_cargo_command_runs_in_rust_repos_only() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().display().to_string();
        let rust = temp.path().join("rust").display().to_string();
        let docs = temp.path().join("docs").display().to_string();
        write(
            temp.path().join("rust/Cargo.toml"),
            "[package]\nname = \"rust\"\n",
        );
        std::fs::create_dir_all(temp.path().join("docs")).unwrap();

        let commands = plan(
            &["fmt", "--check", "--", "--config", "a b"],
            &[&root, &rust, &docs],
            temp.path(),
        );

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].dir, rust);
        assert_eq!(commands[0].cmd, "cargo fmt --check -- --config 'a b'");
    }

    #[test]
    fn workspace_members_run_once_from_the_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().display().to_string();
        let member = temp.path().join("member").display().to_string();
        let own = temp.path().join("own").display().to_string();
        write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\nexclude = [\"own\"]\n",
        );
        for name in ["member", "own"] {
            write(
                temp.path().join(name).join("Cargo.toml"),
                &format!("[package]\nname = \"{name}\"\n"),
            );
            write(temp.path().join(name).join("src/lib.rs"), "");
        }

        let commands = plan(&["--version"], &[&root, &member, &own], temp.path());
        let dirs: Vec<&str> = commands.iter().map(|c| c.dir.as_str()).collect();
        assert_eq!(dirs, [root.as_str(), own.as_str()]);

        // With the root filtered out, the member runs on its own.
        let options = PluginRequestOptions {
            include_filters: Some(vec!["member".to_string()]),
            ..Default::default()
        };
        let args = vec!["--version".to_string()];
        let projects = vec![root, member.clone(), own];
        match execute_command(&args, &options, &projects, temp.path()) {
            CommandResult::Plan(commands, _) => {
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0].dir, member);
            }
            _ => panic!("Expected Plan result"),
        }
    }
}
