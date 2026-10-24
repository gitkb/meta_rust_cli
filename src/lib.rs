//! meta-rust library
//!
//! Runs any cargo command across the Rust repos of a meta workspace.

pub use meta_plugin_protocol::{CommandResult, PlannedCommand, PluginRequestOptions};
use std::path::Path;

/// Plan `cargo <args>` in every selected repo that has a `Cargo.toml`.
///
/// `projects` is the repo list from meta_cli (it includes the meta root);
/// when it is empty the plan runs in `cwd` only. A Cargo workspace that spans
/// repos runs in each of them; plain `cargo` in its root runs it once.
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
    let (rust_dirs, other_dirs): (Vec<String>, Vec<String>) = filter_dirs(dirs, options)
        .into_iter()
        .partition(|dir| cwd.join(dir).join("Cargo.toml").is_file());

    if !options.silent && !other_dirs.is_empty() {
        let names: Vec<String> = other_dirs.iter().map(|dir| display(dir, &root)).collect();
        eprintln!("meta-rust: skipping {} (no Cargo.toml)", names.join(", "));
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

/// Keep the dirs loop_lib will run in (its substring match), so the skip note
/// names only selected repos.
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

    fn write(dir: &Path, file: &str, contents: &str) {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn plan(args: &[&str], projects: &[&Path], options: &PluginRequestOptions) -> Vec<String> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let projects: Vec<String> = projects.iter().map(|p| p.display().to_string()).collect();
        match execute_command(&args, options, &projects, Path::new(".")) {
            CommandResult::Plan(commands, _) => commands
                .into_iter()
                .map(|c| format!("{}: {}", c.dir, c.cmd))
                .collect(),
            _ => panic!("Expected Plan result"),
        }
    }

    #[test]
    fn any_cargo_command_runs_in_rust_repos_only() {
        let temp = TempDir::new().unwrap();
        let (root, rust, docs) = (
            temp.path(),
            temp.path().join("rust"),
            temp.path().join("docs"),
        );
        write(&rust, "Cargo.toml", "[package]\nname = \"rust\"\n");
        write(&docs, "README.md", "");

        let args = ["fmt", "--check", "--", "--config", "a b"];
        let commands = plan(&args, &[root, &rust, &docs], &Default::default());

        let expected = format!("{}: cargo fmt --check -- --config 'a b'", rust.display());
        assert_eq!(commands, [expected]);
    }

    #[test]
    fn workspace_members_run_in_their_own_repos_within_filters() {
        let temp = TempDir::new().unwrap();
        let (root, member) = (temp.path(), temp.path().join("member"));
        write(root, "Cargo.toml", "[workspace]\nmembers = [\"member\"]\n");
        write(&member, "Cargo.toml", "[package]\nname = \"member\"\n");

        let all = plan(&["run"], &[root, &member], &Default::default());
        let only_member = PluginRequestOptions {
            include_filters: Some(vec!["member".to_string()]),
            ..Default::default()
        };
        let filtered = plan(&["run"], &[root, &member], &only_member);

        let run_in = |dir: &Path| format!("{}: cargo run", dir.display());
        assert_eq!(all, [run_in(root), run_in(&member)]);
        assert_eq!(filtered, [run_in(&member)]);
    }
}
