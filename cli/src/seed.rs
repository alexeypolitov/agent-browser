use crate::native::cdp::chrome::{
    copy_chrome_profile_into, find_chrome_user_data_dir, resolve_chrome_profile,
    skip_copied_profile_entry,
};
use crate::validation;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const SEED_METADATA: &str = "seed.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedMetadata {
    pub name: String,
    pub profile_directory: String,
    pub source_profile: String,
    pub created_at: String,
}

pub fn seeds_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".agent-browser")
        .join("seeds")
}

pub fn seed_path(name: &str) -> PathBuf {
    seeds_dir().join(name)
}

pub fn validate_seed_name(name: &str) -> Result<(), String> {
    if validation::is_valid_session_name(name) {
        Ok(())
    } else {
        Err(format!(
            "Invalid seed name '{}'. Only alphanumeric characters, hyphens, and underscores are allowed.",
            name
        ))
    }
}

/// Drop the non-explicit side when `--seed` and `--profile` both resolve.
/// Error only when both remain (both CLI, or both env/config).
pub fn resolve_seed_and_profile(
    seed: &mut Option<String>,
    profile: &mut Option<String>,
    seed_explicit: bool,
    profile_explicit: bool,
) -> Result<(), String> {
    if seed.as_ref().is_some_and(|s| s.is_empty()) {
        *seed = None;
    }
    if profile.as_ref().is_some_and(|s| s.is_empty()) {
        *profile = None;
    }
    if seed.is_none() || profile.is_none() {
        return Ok(());
    }
    if seed_explicit && !profile_explicit {
        *profile = None;
        return Ok(());
    }
    if profile_explicit && !seed_explicit {
        *seed = None;
        return Ok(());
    }
    Err(conflict_error(
        seed.as_deref(),
        profile.as_deref(),
        seed_explicit,
        profile_explicit,
    ))
}

pub fn conflict_error(
    seed: Option<&str>,
    profile: Option<&str>,
    seed_explicit: bool,
    profile_explicit: bool,
) -> String {
    format!(
        "Cannot use --seed with --profile.\n  seed: {}\n  profile: {}\nOmit --profile to clone the seed, or pass --no-seed to use --profile.",
        source_label(seed, seed_explicit, "AGENT_BROWSER_SEED", "--seed"),
        source_label(
            profile,
            profile_explicit,
            "AGENT_BROWSER_PROFILE",
            "--profile"
        ),
    )
}

fn source_label(value: Option<&str>, explicit: bool, env_name: &str, flag: &str) -> String {
    match value {
        None => "unset".to_string(),
        Some(v) if explicit => format!("CLI {flag} {v}"),
        Some(v) => format!("env {env_name}={v}"),
    }
}

pub fn mcp_defaults() -> Value {
    let session = env::var("AGENT_BROWSER_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let namespace = env::var("AGENT_BROWSER_NAMESPACE")
        .ok()
        .filter(|s| !s.is_empty());
    let seed = env::var("AGENT_BROWSER_SEED")
        .ok()
        .filter(|s| !s.is_empty());
    let profile = env::var("AGENT_BROWSER_PROFILE")
        .ok()
        .filter(|s| !s.is_empty());
    let headed = env_truthy("AGENT_BROWSER_HEADED");
    let restore = env::var("AGENT_BROWSER_RESTORE")
        .ok()
        .filter(|s| !s.is_empty());

    let (seed_status, source_profile) = match seed.as_deref() {
        Some(name) => match read_metadata(&seed_path(name)) {
            Ok(meta) => (Some("ok"), Some(meta.source_profile)),
            Err(_) => (Some("missing"), None),
        },
        None => (None, None),
    };

    json!({
        "session": session,
        "namespace": namespace,
        "seed": seed,
        "seedStatus": seed_status,
        "sourceProfile": source_profile,
        "profile": profile,
        "headed": headed,
        "restore": restore,
    })
}

pub fn mcp_defaults_instructions() -> String {
    let d = mcp_defaults();
    let session = d["session"].as_str().unwrap_or("default");
    let mut parts = vec![format!("session={session}")];
    if let Some(ns) = d["namespace"].as_str() {
        parts.push(format!("namespace={ns}"));
    }
    if let Some(seed) = d["seed"].as_str() {
        let status = match d["seedStatus"].as_str() {
            Some("ok") => {
                if let Some(src) = d["sourceProfile"].as_str() {
                    format!("ok, from profile {src}")
                } else {
                    "ok".to_string()
                }
            }
            _ => {
                format!("MISSING — run: agent-browser seed save {seed} --profile <chrome-profile>")
            }
        };
        parts.push(format!("seed={seed} ({status})"));
    }
    if let Some(profile) = d["profile"].as_str() {
        parts.push(format!("profile={profile}"));
    }
    if d["headed"].as_bool() == Some(true) {
        parts.push("headed".to_string());
    }
    if let Some(restore) = d["restore"].as_str() {
        parts.push(format!("restore={restore}"));
    }

    let mut line = format!("Defaults: {}.", parts.join(" "));
    if d["seed"].is_string() && d["seedStatus"].as_str() == Some("ok") {
        line.push_str(
            " Do not pass --profile; each session clones this seed. Override with tool seed/profile or extraArgs --no-seed.",
        );
    } else if d["profile"].is_string() {
        line.push_str(
            " Using --profile (exclusive; prefer --seed for parallel logged-in sessions).",
        );
    } else if !d["seed"].is_string() {
        line.push_str(" No login seed/profile.");
    }
    line
}

fn env_truthy(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref().map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

pub fn read_metadata(seed_dir: &Path) -> Result<SeedMetadata, String> {
    let path = seed_dir.join(SEED_METADATA);
    let data = fs::read_to_string(&path)
        .map_err(|e| format!("Seed metadata missing at {}: {}", path.display(), e))?;
    serde_json::from_str(&data).map_err(|e| format!("Invalid seed metadata: {}", e))
}

pub fn save_seed(name: &str, source_profile: &str) -> Result<SeedMetadata, String> {
    validate_seed_name(name)?;
    let user_data_dir = find_chrome_user_data_dir().ok_or_else(|| {
        "No Chrome user data directory found. Cannot snapshot a profile.".to_string()
    })?;
    let profile_directory = resolve_chrome_profile(&user_data_dir, source_profile)?;
    let dest = seed_path(name);
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|e| format!("Failed to replace existing seed {}: {}", name, e))?;
    }
    fs::create_dir_all(&dest).map_err(|e| format!("Failed to create seed dir: {}", e))?;
    copy_chrome_profile_into(&user_data_dir, &profile_directory, &dest)?;
    let meta = SeedMetadata {
        name: name.to_string(),
        profile_directory,
        source_profile: source_profile.to_string(),
        created_at: chrono_now(),
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("Failed to serialize seed metadata: {}", e))?;
    fs::write(dest.join(SEED_METADATA), json)
        .map_err(|e| format!("Failed to write seed metadata: {}", e))?;
    Ok(meta)
}

pub fn list_seeds() -> Vec<SeedMetadata> {
    let dir = seeds_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut seeds = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if let Ok(meta) = read_metadata(&entry.path()) {
            seeds.push(meta);
        }
    }
    seeds.sort_by(|a, b| a.name.cmp(&b.name));
    seeds
}

pub fn delete_seed(name: &str) -> Result<(), String> {
    validate_seed_name(name)?;
    let dest = seed_path(name);
    if !dest.exists() {
        return Err(format!("Seed '{}' not found", name));
    }
    fs::remove_dir_all(&dest).map_err(|e| format!("Failed to delete seed {}: {}", name, e))
}

pub fn clone_seed_to_temp(name: &str) -> Result<(PathBuf, SeedMetadata), String> {
    validate_seed_name(name)?;
    let src = seed_path(name);
    if !src.is_dir() {
        return Err(format!(
            "Seed '{}' not found. Run `agent-browser seed save {} --profile <name>` first.",
            name, name
        ));
    }
    let meta = read_metadata(&src)?;
    let temp_dir = std::env::temp_dir().join(format!(
        "agent-browser-seed-{}-{}",
        name,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp seed dir: {}", e))?;
    copy_dir_best_effort(&src, &temp_dir)?;
    Ok((temp_dir, meta))
}

fn copy_dir_best_effort(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create directory {}: {}", dst.display(), e))?;
    let entries = fs::read_dir(src)
        .map_err(|e| format!("Failed to read directory {}: {}", src.display(), e))?;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Warning: failed to read entry in {}: {}",
                    src.display(),
                    e
                );
                continue;
            }
        };
        let src_path = entry.path();
        let name = entry.file_name();
        if skip_copied_profile_entry(&name.to_string_lossy()) {
            continue;
        }
        let dst_path = dst.join(&name);
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            copy_dir_best_effort(&src_path, &dst_path)?;
        } else if let Err(e) = fs::copy(&src_path, &dst_path) {
            let _ = writeln!(
                std::io::stderr(),
                "Warning: failed to copy {}: {}",
                src_path.display(),
                e
            );
        }
    }
    Ok(())
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

pub fn run_seed(args: &[String], json_mode: bool, profile_flag_value: Option<&str>) {
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => run_list(json_mode),
        "save" => {
            let name = match args.get(2) {
                Some(n) => n,
                None => {
                    fail(
                        json_mode,
                        "Usage: agent-browser seed save <name> --profile <chrome-profile>",
                    );
                    return;
                }
            };
            let profile = match profile_flag_value
                .map(str::to_string)
                .or_else(|| profile_flag(args))
            {
                Some(p) => p,
                None => {
                    fail(
                        json_mode,
                        "seed save requires --profile <name>. Example: agent-browser seed save work --profile Default",
                    );
                    return;
                }
            };
            match save_seed(name, &profile) {
                Ok(meta) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "data": meta,
                            })
                        );
                    } else {
                        println!(
                            "Saved seed '{}' from profile '{}' ({})",
                            meta.name, meta.source_profile, meta.profile_directory
                        );
                    }
                }
                Err(e) => fail(json_mode, &e),
            }
        }
        "show" => {
            let name = match args.get(2) {
                Some(n) => n,
                None => {
                    fail(json_mode, "Usage: agent-browser seed show <name>");
                    return;
                }
            };
            match read_metadata(&seed_path(name)) {
                Ok(meta) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "data": meta,
                            })
                        );
                    } else {
                        println!("Name:               {}", meta.name);
                        println!("Source profile:     {}", meta.source_profile);
                        println!("Profile directory:  {}", meta.profile_directory);
                        println!("Created (unix):     {}", meta.created_at);
                        println!("Path:               {}", seed_path(name).display());
                    }
                }
                Err(e) => fail(json_mode, &e),
            }
        }
        "delete" | "rm" => {
            let name = match args.get(2) {
                Some(n) => n,
                None => {
                    fail(json_mode, "Usage: agent-browser seed delete <name>");
                    return;
                }
            };
            match delete_seed(name) {
                Ok(()) => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "data": { "deleted": name },
                            })
                        );
                    } else {
                        println!("Deleted seed '{}'", name);
                    }
                }
                Err(e) => fail(json_mode, &e),
            }
        }
        other => fail(
            json_mode,
            &format!(
                "Unknown seed subcommand '{}'. Use list, save, show, or delete.",
                other
            ),
        ),
    }
}

fn run_list(json_mode: bool) {
    let seeds = list_seeds();
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "data": { "seeds": seeds },
            })
        );
        return;
    }
    if seeds.is_empty() {
        println!(
            "No seeds saved. Run `agent-browser seed save <name> --profile <chrome-profile>`."
        );
        return;
    }
    println!("Seeds:");
    for s in &seeds {
        println!(
            "  {}  (from {} / {})",
            s.name, s.source_profile, s.profile_directory
        );
    }
}

fn profile_flag(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--profile" {
            return args.get(i + 1).cloned();
        }
        if let Some(value) = args[i].strip_prefix("--profile=") {
            return Some(value.to_string());
        }
        i += 1;
    }
    None
}

fn fail(json_mode: bool, message: &str) {
    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "success": false,
                "error": message,
            })
        );
    } else {
        eprintln!("{}", message);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_seed_name() {
        assert!(validate_seed_name("work").is_ok());
        assert!(validate_seed_name("work-login_1").is_ok());
        assert!(validate_seed_name("bad name").is_err());
        assert!(validate_seed_name("").is_err());
    }

    #[test]
    fn test_cli_profile_overrides_env_seed() {
        let mut seed = Some("work".to_string());
        let mut profile = Some("Default".to_string());
        resolve_seed_and_profile(&mut seed, &mut profile, false, true).unwrap();
        assert!(seed.is_none());
        assert_eq!(profile.as_deref(), Some("Default"));
    }

    #[test]
    fn test_cli_seed_overrides_env_profile() {
        let mut seed = Some("work".to_string());
        let mut profile = Some("Default".to_string());
        resolve_seed_and_profile(&mut seed, &mut profile, true, false).unwrap();
        assert_eq!(seed.as_deref(), Some("work"));
        assert!(profile.is_none());
    }

    #[test]
    fn test_both_cli_seed_and_profile_error() {
        let mut seed = Some("work".to_string());
        let mut profile = Some("Default".to_string());
        let err = resolve_seed_and_profile(&mut seed, &mut profile, true, true).unwrap_err();
        assert!(err.contains("Cannot use --seed with --profile"));
        assert!(err.contains("CLI --seed work"));
        assert!(err.contains("CLI --profile Default"));
        assert!(err.contains("--no-seed"));
        assert_eq!(seed.as_deref(), Some("work"));
        assert_eq!(profile.as_deref(), Some("Default"));
    }

    #[test]
    fn test_both_env_seed_and_profile_error() {
        let mut seed = Some("work".to_string());
        let mut profile = Some("Default".to_string());
        let err = resolve_seed_and_profile(&mut seed, &mut profile, false, false).unwrap_err();
        assert!(err.contains("env AGENT_BROWSER_SEED=work"));
        assert!(err.contains("env AGENT_BROWSER_PROFILE=Default"));
    }

    #[test]
    fn test_mcp_defaults_instructions_include_seed() {
        use crate::test_utils::EnvGuard;
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_SESSION",
            "AGENT_BROWSER_NAMESPACE",
            "AGENT_BROWSER_SEED",
            "AGENT_BROWSER_PROFILE",
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_RESTORE",
        ]);
        guard.set("AGENT_BROWSER_SESSION", "work");
        guard.set("AGENT_BROWSER_NAMESPACE", "work");
        guard.set("AGENT_BROWSER_SEED", "work");
        guard.remove("AGENT_BROWSER_PROFILE");
        let text = mcp_defaults_instructions();
        assert!(text.contains("session=work"), "{text}");
        assert!(text.contains("namespace=work"), "{text}");
        assert!(text.contains("seed=work"), "{text}");
        assert!(
            text.contains("MISSING") || text.contains("Do not pass --profile"),
            "{text}"
        );
    }

    #[test]
    fn test_copy_dir_best_effort_skips_seed_json_and_locks() {
        let src = std::env::temp_dir().join(format!("seed-copy-src-{}", uuid::Uuid::new_v4()));
        let dst = std::env::temp_dir().join(format!("seed-copy-dst-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(src.join("Profile 49")).unwrap();
        fs::write(src.join("seed.json"), b"meta").unwrap();
        fs::write(src.join("Local State"), b"state").unwrap();
        fs::write(src.join("Profile 49").join("Cookies"), b"keep").unwrap();
        fs::write(src.join("Profile 49").join("Current Session"), b"tabs").unwrap();
        fs::write(src.join("Profile 49").join("LOCK"), b"lock").unwrap();

        copy_dir_best_effort(&src, &dst).unwrap();

        assert!(!dst.join("seed.json").exists());
        assert_eq!(fs::read(dst.join("Local State")).unwrap(), b"state");
        assert_eq!(
            fs::read(dst.join("Profile 49").join("Cookies")).unwrap(),
            b"keep"
        );
        assert!(!dst.join("Profile 49").join("Current Session").exists());
        assert!(!dst.join("Profile 49").join("LOCK").exists());

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }
}
