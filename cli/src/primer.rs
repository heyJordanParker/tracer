//! The session-start primer `trace context` prints with no arguments: eight
//! sections (environment, identity, tech stack, layout, common directories,
//! git, rules, spine) plus the `repo_context` footer. Its first run warms the
//! file cache and the relations index.
//!
//! CCN is AST-derived; the Layout per-path aggregation uses the real
//! `file_facts::get` (no lite-facts shortcut).

use crate::commands::paths_match;
use crate::session;
use crate::git_activity::git_str;
use crate::{cache, docs_graph, file_facts, git_activity, relations, repo_context, repo_files, summary};
use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const PRIMER_LANGUAGE_LIMIT: usize = 10;
const PRIMER_DIRTY_LIMIT: usize = 10;
const PRIMER_COMMIT_LIMIT: usize = 10;
const PRIMER_SPINE_LIMIT: usize = 10;
const PRIMER_APPLICABLE_RULES_LIMIT: usize = 10;
const PRIMER_BRANCH_STALE_DAYS: i64 = 21;

const FEATURE_PREFIXES: &[&str] = &[
    "feat/",
    "feature/",
    "fix/",
    "bugfix/",
    "hotfix/",
    "chore/",
    "refactor/",
    "docs/",
    "test/",
    "tests/",
    "ci/",
    "build/",
    "wip/",
    "experiment/",
    "spike/",
    "release/",
];

/// (filename, manager label). List order is significant: every present
/// config is collected, and this order determines the per-manager filename
/// order in the output.
const PACKAGE_CONFIGS: &[(&str, &str)] = &[
    ("package.json", "npm/node"),
    ("package-lock.json", "npm"),
    ("yarn.lock", "yarn"),
    ("pnpm-lock.yaml", "pnpm"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
    ("composer.json", "composer"),
    ("composer.lock", "composer"),
    ("Gemfile", "bundler"),
    ("Gemfile.lock", "bundler"),
    ("Cargo.toml", "cargo"),
    ("Cargo.lock", "cargo"),
    ("pyproject.toml", "pip/poetry/hatch"),
    ("requirements.txt", "pip"),
    ("Pipfile", "pipenv"),
    ("Pipfile.lock", "pipenv"),
    ("go.mod", "go modules"),
    ("go.sum", "go modules"),
    ("Brewfile", "homebrew"),
    ("build.gradle", "gradle"),
    ("build.gradle.kts", "gradle"),
    ("pom.xml", "maven"),
    ("Package.swift", "swift package manager"),
    ("mix.exs", "mix"),
    ("deno.json", "deno"),
];

const TOOL_CONFIGS: &[&str] = &[
    "vite.config.js",
    "vite.config.ts",
    "webpack.config.js",
    "webpack.config.ts",
    "rollup.config.js",
    "rollup.config.ts",
    "esbuild.config.js",
    "esbuild.config.ts",
    "tsconfig.json",
    "jsconfig.json",
    "playwright.config.ts",
    "playwright.config.js",
    "vitest.config.ts",
    "vitest.config.js",
    "jest.config.js",
    "jest.config.ts",
    "phpunit.xml",
    "phpunit.xml.dist",
    "pytest.ini",
    "tox.ini",
    "biome.json",
    ".eslintrc.json",
    ".eslintrc.js",
    "prettier.config.js",
    ".prettierrc",
    "pint.json",
    ".rubocop.yml",
    "Dockerfile",
    "docker-compose.yml",
    "compose.yaml",
    "compose.yml",
    ".lando.yml",
    "wp-cli.yml",
    "Makefile",
];

const CI_MARKERS: &[(&str, &str)] = &[
    (".github/workflows", "GitHub Actions"),
    (".gitlab-ci.yml", "GitLab CI"),
    (".circleci/config.yml", "CircleCI"),
    ("Jenkinsfile", "Jenkins"),
    ("azure-pipelines.yml", "Azure Pipelines"),
    ("bitbucket-pipelines.yml", "Bitbucket Pipelines"),
    (".drone.yml", "Drone"),
    (".travis.yml", "Travis"),
];

const TEST_CONFIG_NAMES: &[&str] = &[
    "phpunit.xml",
    "phpunit.xml.dist",
    "pytest.ini",
    "tox.ini",
    "vitest.config.ts",
    "vitest.config.js",
    "jest.config.js",
    "jest.config.ts",
    "playwright.config.ts",
    "playwright.config.js",
];

const COMMON_KINDS: &[&str] = &[
    "frontend",
    "backend",
    "database-migrations",
    "tests",
    "scripts",
    "continuous-integration",
];

const DIRTY_STATES: &[&str] = &["untracked", "added", "modified", "renamed"];

pub fn run() -> Result<()> {
    let here = Path::new(".");
    let repo_root = cache::worktree_root_for(here).unwrap_or_else(|| cache::display_root(here));
    let index = relations::get(&repo_root);
    print!("{}", sections(&repo_root, Some(&index)).into_iter().map(|section| format!("{section}\n")).collect::<String>());
    if let Some(footer) = repo_context::footer(&repo_root, None) {
        println!("{footer}");
    }
    Ok(())
}

/// The primer a session starts with, as extra context in `room`: offered as
/// the repository's `Primer`, one line per section keyed by its heading, so
/// a resumed session that holds it gets nothing and a repository that
/// changed since gets only the sections that changed. Sections that don't
/// fit are left out from the last, and stay unshown. The index builds until
/// the hook's deadline leaves only room to print, and the parts that need it
/// are left out when it isn't ready.
pub fn delivered(repo_root: &Path, room: usize, delivery: &mut crate::session::Delivery) -> String {
    use crate::session::{Kind, Offer};
    let key = repo_root.to_string_lossy();
    let index = relations::within(repo_root, crate::process::warm_limit());
    let sections: Vec<(String, String)> = sections(repo_root, index.as_deref())
        .into_iter()
        .filter(|section| !section.trim().is_empty())
        .map(|section| (section.lines().next().unwrap_or_default().to_string(), format!("{section}\n")))
        .collect();
    let offered = |shown: &[&(String, String)]| {
        serde_json::Value::Object(shown.iter().map(|(heading, text)| (heading.clone(), text.clone().into())).collect())
    };
    let unshown: Vec<&(String, String)> = match delivery.offer(Kind::Primer, &key, &offered(&sections.iter().collect::<Vec<_>>())) {
        Offer::Same => Vec::new(),
        Offer::New => sections.iter().collect(),
        Offer::Changed(lines) => sections.iter().filter(|(heading, _)| lines.contains_key(heading)).collect(),
    };
    let footer = repo_context::footer(repo_root, Some(delivery)).map(|line| format!("{line}\n")).unwrap_or_default();
    let mut used = crate::output::width(&footer);
    let fits: Vec<&(String, String)> = unshown
        .iter()
        .copied()
        .take_while(|(_, text)| {
            used += crate::output::width(text);
            used <= room
        })
        .collect();
    if fits.len() < unshown.len() {
        delivery.withdraw(Kind::Primer, &key);
        if !fits.is_empty() {
            let _ = delivery.offer(Kind::Primer, &key, &offered(&fits));
        }
    }
    let mut out: String = fits.iter().map(|(_, text)| text.as_str()).collect();
    out.push_str(&footer);
    out
}

/// Every section, with the ones that need `index` left out without it.
fn sections(repo_root: &Path, index: Option<&relations::Relations>) -> Vec<String> {
    let repo_root = repo_root.to_path_buf();
    let tracked = repo_files::tracked_files(&repo_root, None).unwrap_or_default();

    // Each section is an independent, self-contained read of repo state —
    // git subprocesses, the relations index, scc facts. They share no mutable
    // state, so they run concurrently; the per-section git-spawn cost is the
    // primer's whole budget, and running the sections in parallel collapses
    // the serial sum to the slowest single section. Output is assembled in
    // fixed order below, so the bytes are identical to serial emission.
    thread::scope(|scope| {
        let environment = scope.spawn(|| environment_section(&repo_root));
        let identity = scope.spawn(|| identity_section(&repo_root));
        let tech_stack = scope.spawn(|| tech_stack_section(&repo_root));
        let layout = scope.spawn(|| layout_section(&repo_root, &tracked));
        let common_directories = scope.spawn(|| common_directories_section(&repo_root, &tracked));
        let git = scope.spawn(|| git_section(&repo_root, index));
        let rules = scope.spawn(|| rules_section(&repo_root, &tracked));
        let spine = scope.spawn(|| index.map(spine_section).unwrap_or_default());
        vec![
            environment.join().unwrap(),
            identity.join().unwrap(),
            tech_stack.join().unwrap(),
            layout.join().unwrap(),
            common_directories.join().unwrap(),
            git.join().unwrap(),
            rules.join().unwrap(),
            spine.join().unwrap(),
        ]
    })
}

// --- Section: Environment ----------------------------------------------

fn environment_section(repo_root: &Path) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_git = repo_root.join(".git").exists();
    let is_worktree = worktree(repo_root);
    let shell = std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(|x| x.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(unknown)".into());
    // git_user spawns two `git config` reads; os_release spawns `uname`.
    // Independent — run them concurrently.
    let (git_user, os_release) = thread::scope(|scope| {
        let git_user = scope.spawn(|| git_user(repo_root));
        let os_release = scope.spawn(os_release);
        (git_user.join().unwrap(), os_release.join().unwrap())
    });
    let date = unix_to_ymd(now_secs());

    let mut out = String::new();
    let _ = writeln!(out, "## Environment");
    let _ = writeln!(out, "  cwd: {cwd}");
    let _ = writeln!(out, "  repo root: {}", repo_root.to_string_lossy());
    let _ = writeln!(
        out,
        "  git repository: {}",
        if is_git { "yes" } else { "no" }
    );
    let _ = writeln!(
        out,
        "  worktree: {}",
        if is_worktree { "yes" } else { "no" }
    );
    let _ = writeln!(out, "  platform: {}", os_system().to_lowercase());
    let _ = writeln!(out, "  shell: {shell}");
    let _ = writeln!(out, "  os version: {} {}", os_system(), os_release);
    let _ = writeln!(out, "  git user: {git_user}");
    let _ = writeln!(out, "  date: {date}");
    out
}

/// Host OS name: "Darwin" / "Linux" / "Windows".
fn os_system() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    }
}

fn os_release() -> String {
    crate::process::output(Command::new("uname").arg("-r"))
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn worktree(repo_root: &Path) -> bool {
    let git_path = repo_root.join(".git");
    if !git_path.is_file() {
        return false;
    }
    std::fs::read_to_string(&git_path)
        .map(|c| c.contains("worktrees/"))
        .unwrap_or(false)
}

fn git_user(repo_root: &Path) -> String {
    let (name, email) = thread::scope(|scope| {
        let name = scope
            .spawn(|| git_str(repo_root, &["config", "--get", "user.name"]).unwrap_or_default());
        let email = scope
            .spawn(|| git_str(repo_root, &["config", "--get", "user.email"]).unwrap_or_default());
        (name.join().unwrap(), email.join().unwrap())
    });
    if !name.is_empty() && !email.is_empty() {
        format!("{name} <{email}>")
    } else if !name.is_empty() {
        name
    } else if !email.is_empty() {
        email
    } else {
        "(unset)".into()
    }
}

// --- Section: Identity --------------------------------------------------

fn identity_section(repo_root: &Path) -> String {
    let languages = repo_context::language_summary(repo_root);
    let mut out = String::new();
    let _ = writeln!(out, "## Identity");
    if languages.is_empty() {
        let _ = writeln!(out, "  (scc unavailable or empty result)");
        return out;
    }
    let total_files: i64 = languages.iter().map(|language| language.count).sum();
    let total_loc: i64 = languages.iter().map(|language| language.code).sum();
    let mut sorted = languages.clone();
    sorted.sort_by(|a, b| b.code.cmp(&a.code));

    let _ = writeln!(out, "  Files: {total_files}  Lines of code: {total_loc}");
    let _ = writeln!(out, "  Languages:");
    for lang in sorted.iter().take(PRIMER_LANGUAGE_LIMIT) {
        let facts = serde_json::json!({
            "files": lang.count,
            "lines_of_code": lang.code,
            "cyclomatic_complexity": lang.complexity,
        });
        let _ = writeln!(out, "    {:<20} {}", lang.name, crate::yamlfmt::flow(&facts, false));
    }
    let extra = sorted.len() as i64 - PRIMER_LANGUAGE_LIMIT as i64;
    if extra > 0 {
        let _ = writeln!(out, "    … {extra} more languages");
    }
    out
}

// --- Section: Tech Stack ------------------------------------------------

fn tech_stack_section(repo_root: &Path) -> String {
    let mut managers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (filename, manager) in PACKAGE_CONFIGS {
        if repo_root.join(filename).exists() {
            managers
                .entry(manager.to_string())
                .or_default()
                .push(filename.to_string());
        }
    }
    let configs: Vec<&str> = TOOL_CONFIGS
        .iter()
        .copied()
        .filter(|f| repo_root.join(f).exists())
        .collect();

    let mut out = String::new();
    let _ = writeln!(out, "## Tech Stack");
    if !managers.is_empty() {
        let _ = writeln!(out, "  Package managers:");
        for (manager, files) in &managers {
            let _ = writeln!(out, "    {manager}: {}", files.join(", "));
        }
    }
    if !configs.is_empty() {
        let _ = writeln!(out, "  Build / test / lint configs:");
        for config in &configs {
            let _ = writeln!(out, "    {config}");
        }
    }
    if managers.is_empty() && configs.is_empty() {
        let _ = writeln!(out, "  (no package or tool configs detected at repo root)");
    }
    out
}

// --- Section: Layout ----------------------------------------------------

fn layout_section(repo_root: &Path, tracked: &[String]) -> String {
    let skip = repo_files::skip_dirs();
    let mut by_top: BTreeMap<String, (usize, i64, Option<String>, bool)> = BTreeMap::new();
    let git_map = git_activity::bulk_cached(repo_root);
    for rel in tracked {
        let head = match rel.split_once('/') {
            Some((h, _)) => h,
            None => continue,
        };
        if head.starts_with('.') || skip.contains(head) {
            continue;
        }
        let summary = by_top.entry(head.to_string()).or_default();
        summary.0 += 1;
        if let Some(activity) = git_map.get(rel) {
            if let Some(modified) = &activity.last_modified {
                if summary
                    .2
                    .as_ref()
                    .map(|latest| modified > latest)
                    .unwrap_or(true)
                {
                    summary.2 = Some(modified.clone());
                }
            }
            if activity
                .working_state
                .as_deref()
                .map(|state| DIRTY_STATES.contains(&state))
                .unwrap_or(false)
            {
                summary.3 = true;
            }
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "## Layout");
    if by_top.is_empty() {
        let _ = writeln!(out, "  (no source directories at top level)");
        return out;
    }

    let source_exts: std::collections::HashSet<&str> = crate::extraction::supported_extensions()
        .iter()
        .copied()
        .collect();
    for chunk in tracked.chunks(file_facts::RESOLVE_CHUNK) {
        let source: Vec<PathBuf> = chunk
            .iter()
            .filter(|rel| {
                Path::new(rel)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| source_exts.contains(extension.to_lowercase().as_str()))
                    .unwrap_or(false)
            })
            .map(|rel| repo_root.join(rel))
            .collect();
        let facts = file_facts::get_batch(&source, repo_root);
        for (rel, fact) in facts {
            if let Some((head, _)) = rel.split_once('/') {
                if let Some(summary) = by_top.get_mut(head) {
                    summary.1 += fact.cyclomatic_complexity_total;
                }
            }
        }
    }

    // Case-insensitive sort by lowercased key.
    let mut names: Vec<&String> = by_top.keys().collect();
    names.sort_by_key(|n| n.to_lowercase());

    for name in names {
        let (files, complexity, last_commit, uncommitted) = &by_top[name];
        let mut facts = Map::new();
        facts.insert("files".into(), (*files).into());
        facts.insert("cyclomatic_complexity".into(), (*complexity).into());
        if let Some(last_commit) = last_commit {
            facts.insert("last_commit".into(), last_commit.clone().into());
        }
        if *uncommitted {
            facts.insert("uncommitted".into(), true.into());
        }
        let _ = writeln!(out, "  📁 {name}/  {}", crate::yamlfmt::flow(&Value::Object(facts), false));
    }
    out
}

// --- Section: Common Directories ---------------------------------------

fn common_directories_section(repo_root: &Path, tracked: &[String]) -> String {
    let skip = repo_files::skip_dirs();
    let mut classifications: BTreeMap<&str, Vec<(String, String)>> =
        COMMON_KINDS.iter().map(|k| (*k, Vec::new())).collect();

    for (marker, label) in CI_MARKERS {
        if repo_root.join(marker).exists() {
            classifications
                .get_mut("continuous-integration")
                .unwrap()
                .push((marker.to_string(), label.to_string()));
        }
    }

    // Each directory one or two levels down, with its direct files, from
    // git's file listing — the Layout's own source. Worktrees, nested clones,
    // submodules and ignored checkouts are not in that listing, so they
    // never read as this repository's directories.
    let mut by_directory: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for relative in tracked {
        let Some((directory, name)) = relative.rsplit_once('/') else {
            continue;
        };
        let parts: Vec<&str> = directory.split('/').collect();
        if parts.len() > 2 || parts.iter().any(|part| part.starts_with('.') || skip.contains(part)) {
            continue;
        }
        by_directory.entry(directory).or_default().push(name);
    }
    for (directory, names) in &by_directory {
        for (kind, marker) in classify_directory(&repo_root.join(directory), names) {
            classifications
                .get_mut(kind)
                .unwrap()
                .push((directory.to_string(), marker));
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "## Common Directories");
    let mut any_found = false;
    for kind in COMMON_KINDS {
        let entries = &classifications[kind];
        if entries.is_empty() {
            continue;
        }
        any_found = true;
        let _ = writeln!(out, "  {kind}:");
        for (path, marker) in entries {
            let _ = writeln!(out, "    {path}  ({marker})");
        }
    }
    if !any_found {
        let _ = writeln!(out, "  (no common directories detected)");
    }
    out
}

/// The kinds a directory's direct files mark it as. `file_names` are its
/// files from git's listing.
fn classify_directory(directory: &Path, file_names: &[&str]) -> Vec<(&'static str, String)> {
    let mut ext_counts: HashMap<String, i64> = HashMap::new();
    for name in file_names {
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| format!(".{}", s.to_lowercase()))
            .unwrap_or_default();
        *ext_counts.entry(ext).or_insert(0) += 1;
    }
    let ec = |e: &str| *ext_counts.get(e).unwrap_or(&0);
    let mut labels: Vec<(&'static str, String)> = Vec::new();

    let frontend = ec(".tsx") + ec(".jsx") + ec(".vue") + ec(".svelte");
    if frontend >= 3 {
        labels.push(("frontend", format!("{frontend} tsx/jsx/vue/svelte files")));
    }

    let mut backend: Vec<String> = Vec::new();
    let name_set: std::collections::HashSet<&str> = file_names.iter().copied().collect();
    if name_set.contains("artisan") {
        backend.push("Laravel".into());
    }
    if ["manage.py", "wsgi.py", "asgi.py"]
        .iter()
        .any(|n| name_set.contains(n))
    {
        backend.push("Django/Flask".into());
    }
    if name_set.contains("config.ru")
        || (name_set.contains("Gemfile")
            && directory.join("config").join("application.rb").exists())
    {
        backend.push("Rails".into());
    }
    let php_count = ec(".php");
    if php_count >= 3
        && file_names.iter().any(|n| {
            n.ends_with("Controller.php") || n.ends_with("Model.php") || n.ends_with("Service.php")
        })
    {
        backend.push(format!("{php_count} PHP files (controller/model/service)"));
    }
    if !backend.is_empty() {
        labels.push(("backend", backend.join(", ")));
    }

    let ts_prefixed = file_names
        .iter()
        .filter(|n| is_timestamp_prefixed(n))
        .count();
    if ts_prefixed >= 2 {
        labels.push((
            "database-migrations",
            format!("{ts_prefixed} timestamp-prefixed files"),
        ));
    }

    let test_configs: Vec<&str> = file_names
        .iter()
        .copied()
        .filter(|n| TEST_CONFIG_NAMES.contains(n))
        .collect();
    if !test_configs.is_empty() {
        labels.push(("tests", format!("config: {}", test_configs.join(", "))));
    } else {
        let test_count = file_names
            .iter()
            .filter(|n| {
                n.contains("_test.")
                    || n.contains(".test.")
                    || n.starts_with("test_")
                    || n.ends_with("Test.php")
                    || n.ends_with("Spec.php")
            })
            .count();
        if test_count >= 2 {
            labels.push(("tests", format!("{test_count} test files")));
        }
    }

    let shell_count = file_names
        .iter()
        .filter(|n| n.ends_with(".sh") || n.ends_with(".bash") || n.ends_with(".zsh"))
        .count();
    if shell_count >= 2 {
        labels.push(("scripts", format!("{shell_count} shell scripts")));
    } else {
        let shebang = count_shebangs(directory, file_names);
        if shebang >= 2 {
            labels.push(("scripts", format!("{shebang} shebang scripts")));
        }
    }

    labels
}

/// `re.match(r"^\d{4}[_-]\d{2}[_-]\d{2}", name)`.
fn is_timestamp_prefixed(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() < 10 {
        return false;
    }
    let d = |i: usize| b[i].is_ascii_digit();
    let sep = |i: usize| b[i] == b'_' || b[i] == b'-';
    d(0) && d(1) && d(2) && d(3) && sep(4) && d(5) && d(6) && sep(7) && d(8) && d(9)
}

fn count_shebangs(directory: &Path, file_names: &[&str]) -> usize {
    let mut count = 0;
    for name in file_names {
        let entry = directory.join(name);
        if !entry.is_file() || entry.extension().is_some() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&entry) {
            if bytes.len() >= 2 && &bytes[..2] == b"#!" {
                count += 1;
            }
        }
    }
    count
}

// --- Section: Git -------------------------------------------------------

fn git_section(repo_root: &Path, index: Option<&relations::Relations>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Git");
    if !repo_root.join(".git").exists() {
        let _ = writeln!(out, "  (not a git repository)");
        return out;
    }

    // Wave 1: four independent git reads run concurrently. Wave 2:
    // candidates needs origin_head, ahead_behind needs origin_head + current,
    // so they resolve once wave 1 lands.
    let (origin_head, current, dirty, commits) = thread::scope(|scope| {
        let origin_head = scope.spawn(|| origin_head_branch(repo_root));
        let current = scope.spawn(|| current_branch(repo_root));
        let dirty = scope.spawn(|| git_activity::working_tree_state(repo_root));
        let commits = scope.spawn(|| recent_commit_subjects(repo_root, PRIMER_COMMIT_LIMIT));
        (
            origin_head.join().unwrap(),
            current.join().unwrap(),
            dirty.join().unwrap(),
            commits.join().unwrap(),
        )
    });
    let (candidates, ahead_behind) = thread::scope(|scope| {
        let candidates =
            scope.spawn(|| primary_branch_candidates(repo_root, origin_head.as_deref()));
        let ahead_behind =
            scope.spawn(|| ahead_behind(repo_root, &current, origin_head.as_deref()));
        (candidates.join().unwrap(), ahead_behind.join().unwrap())
    });

    if !candidates.is_empty() {
        let _ = writeln!(out, "  Primary branch candidates:");
        for (name, info) in &candidates {
            let _ = writeln!(out, "    {name}  ({info})");
        }
    }

    let suffix = if ahead_behind.is_empty() {
        String::new()
    } else {
        format!("  ({ahead_behind})")
    };
    let _ = writeln!(out, "  Current branch: {current}{suffix}");

    if !dirty.is_empty() {
        let _ = writeln!(out, "  Dirty files ({}):", dirty.len());
        for line in render_dirty(repo_root, &dirty, index) {
            let _ = writeln!(out, "    {line}");
        }
    } else {
        let _ = writeln!(out, "  Working tree clean");
    }

    if !commits.is_empty() {
        let _ = writeln!(out, "  Recent commits ({}):", commits.len());
        for line in &commits {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

fn primary_branch_candidates(repo_root: &Path, origin_head: Option<&str>) -> Vec<(String, String)> {
    let stdout = match git_str(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(committerdate:iso8601)",
            "refs/heads/",
            "refs/remotes/origin/",
        ],
    ) {
        Some(s) => s,
        None => return vec![],
    };

    let cutoff = now_secs() - PRIMER_BRANCH_STALE_DAYS * 86400;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut candidates: Vec<(String, String, bool)> = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() != 2 {
            continue;
        }
        let raw_name = parts[0];
        let date_text = parts[1].trim();
        let short = raw_name
            .strip_prefix("origin/")
            .unwrap_or(raw_name)
            .to_string();
        if short == "HEAD" || seen.contains(&short) {
            continue;
        }
        let is_feature = FEATURE_PREFIXES.iter().any(|p| short.starts_with(p));
        let is_origin_head = Some(short.as_str()) == origin_head;
        if is_feature && !is_origin_head {
            continue;
        }
        let branch_secs = parse_iso_secs(date_text);
        let is_stale = branch_secs.map(|s| s < cutoff).unwrap_or(false);
        if is_stale && !is_origin_head {
            continue;
        }
        seen.insert(short.clone());
        let date_short = date_text.split_whitespace().next().unwrap_or("(unknown)");
        let marker = if is_origin_head { " [origin/HEAD]" } else { "" };
        candidates.push((short, format!("last: {date_short}{marker}"), is_origin_head));
    }

    candidates.sort_by(|a, b| {
        let ka = (if a.2 { 0 } else { 1 }, a.0.clone());
        let kb = (if b.2 { 0 } else { 1 }, b.0.clone());
        ka.cmp(&kb)
    });
    candidates.into_iter().map(|(n, i, _)| (n, i)).collect()
}

fn origin_head_branch(repo_root: &Path) -> Option<String> {
    let ref_ = git_str(
        repo_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )?;
    let stripped = ref_.strip_prefix("origin/").unwrap_or(&ref_);
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

fn current_branch(repo_root: &Path) -> String {
    match git_str(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Some(s) if !s.is_empty() => s,
        Some(_) => "(detached)".into(),
        None => "(unknown)".into(),
    }
}

fn ahead_behind(repo_root: &Path, current: &str, base: Option<&str>) -> String {
    let base = match base {
        Some(b) => b,
        None => return String::new(),
    };
    if current == base || current == "(unknown)" || current == "(detached)" {
        return String::new();
    }
    let stdout = match git_str(
        repo_root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("origin/{base}...HEAD"),
        ],
    ) {
        Some(s) => s,
        None => return String::new(),
    };
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() != 2 {
        return String::new();
    }
    format!("ahead: {}, behind: {} vs origin/{base}", parts[1], parts[0])
}

/// The dirty files, most imported first, then most complex. Without `index`
/// the import counts are left out and complexity alone ranks them.
fn render_dirty(repo_root: &Path, dirty: &HashMap<String, String>, index: Option<&relations::Relations>) -> Vec<String> {
    let mut scored: Vec<(Option<i64>, i64, String, String)> = Vec::new();
    let dirty_files: Vec<(&String, &String)> = dirty.iter().collect();
    for chunk in dirty_files.chunks(file_facts::RESOLVE_CHUNK) {
        let existing: Vec<PathBuf> = chunk
            .iter()
            .map(|(path, _)| repo_root.join(path))
            .filter(|absolute| absolute.exists())
            .collect();
        let facts = file_facts::extracted(&existing, repo_root);
        for (path, state) in chunk {
            let callers = index.map(|index| index.resolved_importers_of(path).count() as i64);
            let rel = cache::relative_to_root(&repo_root.join(path), repo_root);
            let ccn = facts
                .get(&rel)
                .map(|fact| fact.cyclomatic_complexity_total)
                .unwrap_or(0);
            scored.push((callers, ccn, (*state).clone(), (*path).clone()));
        }
    }
    // Rank by callers then ccn, both descending. Pre-sort by path so the
    // stable primary/secondary sort is fully deterministic.
    scored.sort_by(|a, b| a.3.cmp(&b.3));
    scored.sort_by_key(|entry| std::cmp::Reverse((entry.0, entry.1)));

    let mut lines: Vec<String> = scored
        .iter()
        .take(PRIMER_DIRTY_LIMIT)
        .map(|(imported_by, complexity, state, path)| {
            let mut facts = serde_json::Map::new();
            if let Some(imported_by) = imported_by {
                facts.insert("imported_by".into(), (*imported_by).into());
            }
            facts.insert("cyclomatic_complexity".into(), (*complexity).into());
            format!("{state:<10} {path}  {}", crate::yamlfmt::flow(&Value::Object(facts), false))
        })
        .collect();
    if scored.len() > PRIMER_DIRTY_LIMIT {
        lines.push(format!("… {} more", scored.len() - PRIMER_DIRTY_LIMIT));
    }
    lines
}

fn recent_commit_subjects(repo_root: &Path, limit: usize) -> Vec<String> {
    let out = crate::git_activity::git_output(
        repo_root,
        ["log", &format!("-n{limit}"), "--pretty=format:%h %s"],
    );
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect(),
        _ => vec![],
    }
}

/// Parse `git for-each-ref` iso8601 ("YYYY-MM-DD HH:MM:SS ±ZZZZ") to unix
/// seconds. Only the date portion is needed for the staleness compare, but
/// the time and UTC offset are honored for an exact instant.
fn parse_iso_secs(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut it = text.split_whitespace();
    let date = it.next()?;
    let time = it.next().unwrap_or("00:00:00");
    let offset = it.next().unwrap_or("+0000");

    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return None;
    }
    let y: i64 = dp[0].parse().ok()?;
    let mo: i64 = dp[1].parse().ok()?;
    let d: i64 = dp[2].parse().ok()?;
    let tp: Vec<&str> = time.split(':').collect();
    let hh: i64 = tp.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mm: i64 = tp.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let ss: i64 = tp.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let days = summary::days_from_civil(y, mo, d);
    let mut secs = days * 86400 + hh * 3600 + mm * 60 + ss;

    // offset like +0200 / -0500 — subtract to get UTC.
    if offset.len() == 5 {
        let sign = if &offset[..1] == "-" { -1 } else { 1 };
        let oh: i64 = offset[1..3].parse().unwrap_or(0);
        let om: i64 = offset[3..5].parse().unwrap_or(0);
        secs -= sign * (oh * 3600 + om * 60);
    }
    Some(secs)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn unix_to_ymd(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// --- Section: Rules -----------------------------------------------------

fn rules_section(repo_root: &Path, tracked: &[String]) -> String {
    // Each `Claude.md` reaches the agent when it reads a file under it, so a
    // list of them here says nothing the agent can act on.
    let rules_files = collect_rules_dir(tracked);

    let mut out = String::new();
    let _ = writeln!(out, "## Rules");
    if !rules_files.is_empty() {
        let _ = writeln!(out, "  Project rules ({}):", rules_files.len());
        for rel in &rules_files {
            let _ = writeln!(out, "    {rel}");
        }
    } else {
        let _ = writeln!(out, "  (no .claude/rules/ found)");
    }

    // Conditional rules whose `paths:` globs match a working-tree-dirty file
    // but that aren't already in the session's context. The plain rules list
    // above tells the agent these files exist; this tells it which ones govern
    // the work it's about to touch, so a constraint is surfaced before it's
    // violated rather than discovered by the violation. Capped so a repo of
    // broad-glob rules can't flood the section.
    let applicable = applicable_unloaded_rules(repo_root);
    if !applicable.is_empty() {
        let _ = writeln!(
            out,
            "  Applies to your changes, not loaded ({}):",
            applicable.len()
        );
        for (rule, glob) in &applicable {
            let _ = writeln!(out, "    {rule} (matches {glob})");
        }
    }
    out
}

/// Conditional rules (`.claude/rules/*.md` with a `paths:` frontmatter) whose
/// globs match at least one working-tree-dirty file and that the agent's
/// record does not already hold. Each entry pairs the rule's path
/// with the glob that matched, so the agent sees why the rule applies. Capped
/// at `PRIMER_APPLICABLE_RULES_LIMIT`.
///
/// The conditional rules and their globs come from the doc graph's nodes
/// (its walk already parsed every rule's frontmatter); the dirty set from the
/// working tree; the loaded set from the agent's record. The three join here at
/// render time only.
fn applicable_unloaded_rules(repo_root: &Path) -> Vec<(String, String)> {
    // The doc graph is its own walk, independent of code relationships. It
    // rode inside the architecture graph only because that entry was where
    // a built structure got stored; built directly it costs one doc-tree
    // walk instead of decoding every code relationship in the repository.
    let docs = docs_graph::build(repo_root);
    let conditional: Vec<&docs_graph::DocNode> = docs
        .nodes
        .iter()
        .filter(|n| {
            n.paths_globs
                .as_ref()
                .map(|g| !g.is_empty())
                .unwrap_or(false)
        })
        .collect();
    if conditional.is_empty() {
        return vec![];
    }

    let dirty = git_activity::working_tree_state(repo_root);
    let dirty_abs: Vec<std::path::PathBuf> = dirty
        .iter()
        .filter(|(_, state)| DIRTY_STATES.contains(&state.as_str()))
        .map(|(rel, _)| repo_root.join(rel))
        .collect();
    if dirty_abs.is_empty() {
        return vec![];
    }

    let loaded = session::loaded_paths();

    let mut out: Vec<(String, String)> = Vec::new();
    for node in conditional {
        // A rule already in context needs no surfacing — the agent has its
        // text. Match against the record's canonical-path keys.
        let rule_abs = repo_root.join(&node.path);
        let canonical = rule_abs
            .canonicalize()
            .unwrap_or(rule_abs)
            .to_string_lossy()
            .to_string();
        if loaded.contains(&canonical) {
            continue;
        }
        let globs = match &node.paths_globs {
            Some(g) => g,
            None => continue,
        };
        // First dirty file × first glob that matches — report the rule once
        // with the glob that triggered it, not once per matching file.
        let mut matched_glob: Option<&String> = None;
        'outer: for file in &dirty_abs {
            for glob in globs {
                if paths_match::matches_paths(file, std::slice::from_ref(glob), repo_root) {
                    matched_glob = Some(glob);
                    break 'outer;
                }
            }
        }
        if let Some(glob) = matched_glob {
            out.push((node.path.clone(), glob.clone()));
        }
    }
    out.sort();
    out.truncate(PRIMER_APPLICABLE_RULES_LIMIT);
    out
}

fn collect_rules_dir(tracked: &[String]) -> Vec<String> {
    let mut out: Vec<String> = tracked
        .iter()
        .filter(|r| r.starts_with(".claude/rules/") && r.ends_with(".md"))
        .cloned()
        .collect();
    out.sort();
    out
}

// --- Section: Spine -----------------------------------------------------

fn spine_section(index: &relations::Relations) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Spine");
    // The same ranking `usages --path` answers with: transitive reach
    // first, direct import edges as the tie-break. `relations` owns it.
    let ranked = relations::ranked_by_reach(index, i64::MAX, PRIMER_SPINE_LIMIT, relations::Reach::Importers, None);
    if ranked.is_empty() {
        let _ = writeln!(
            out,
            "  (no imports found in this repository — run `trace cache build` if you expect data)"
        );
        return out;
    }

    let _ = writeln!(out, "  Top {} most-depended-on nodes:", ranked.len());
    let _ = writeln!(
        out,
        "    {:<3} {:>6} {:>10}  {:<10} symbol @ source",
        "#", "direct", "transitive", "kind"
    );
    for (rank, row) in ranked.iter().enumerate() {
        let language = index.language(&row.file);
        let (label, kind) = match &row.symbol {
            Some(symbol) => (symbol.clone(), "symbol"),
            None => (relations::file_to_module(&row.file, language), "module"),
        };
        let _ = writeln!(
            out,
            "    {:<3} {:>6} {:>10}  {:<10} {} @ {}:1",
            rank + 1,
            row.direct,
            row.transitive,
            kind,
            label,
            row.file,
        );
    }
    out
}
