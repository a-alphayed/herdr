use std::path::{Path, PathBuf};

use tracing::warn;

use super::{model::LoadedConfig, Config, CONFIG_PATH_ENV_VAR};

const KNOWN_TOP_LEVEL_CONFIG_KEYS: &[&str] = &[
    "advanced",
    "experimental",
    "keys",
    "onboarding",
    "remote",
    "session",
    "terminal",
    "theme",
    "ui",
    "update",
    "worktrees",
];

pub fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_config_dir()
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    platform_state_dir()
}

#[cfg(windows)]
fn platform_config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("APPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".config/{}", app_dir_name()));
    }
    std::env::temp_dir().join(app_dir_name())
}

#[cfg(not(windows))]
fn platform_config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".config/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(app_dir_name())
    }
}

#[cfg(windows)]
fn platform_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(dir).join(app_dir_name());
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join(app_dir_name());
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()));
    }
    std::env::temp_dir().join(format!("{}-state", app_dir_name()))
}

#[cfg(not(windows))]
fn platform_state_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".local/state/{}", app_dir_name()))
    } else {
        std::env::temp_dir().join(format!("{}-state", app_dir_name()))
    }
}

impl Config {
    pub fn load() -> LoadedConfig {
        let path = config_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<Config>(&content) {
                    Ok(config) => {
                        let mut diagnostics =
                            unknown_top_level_section_diagnostics_from_str(&content);
                        diagnostics.extend(config.collect_diagnostics());
                        return LoadedConfig {
                            config,
                            diagnostics,
                            invalid_sections: Vec::new(),
                        };
                    }
                    Err(err) => {
                        warn!(err = %err, "config parse error, using defaults");
                        return LoadedConfig {
                            config: Self::default(),
                            diagnostics: vec![format!("config parse error: {err}; using defaults")],
                            invalid_sections: Vec::new(),
                        };
                    }
                },
                Err(err) => {
                    warn!(err = %err, "config read error, using defaults");
                    return LoadedConfig {
                        config: Self::default(),
                        diagnostics: vec![format!("config read error: {err}; using defaults")],
                        invalid_sections: Vec::new(),
                    };
                }
            }
        }
        LoadedConfig {
            config: Self::default(),
            diagnostics: Vec::new(),
            invalid_sections: Vec::new(),
        }
    }
}

pub(super) fn resolve_config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var(CONFIG_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }
    config_dir().join("config.toml")
}

pub fn config_diagnostic_summary(diagnostics: &[String]) -> Option<String> {
    const MAX_VISIBLE_DIAGNOSTICS: usize = 4;

    if diagnostics.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = diagnostics
        .iter()
        .take(MAX_VISIBLE_DIAGNOSTICS)
        .map(|diagnostic| diagnostic.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let hidden = diagnostics.len().saturating_sub(MAX_VISIBLE_DIAGNOSTICS);
    if hidden > 0 {
        lines.push(format!("and {hidden} more config warnings"));
    }
    Some(lines.join("\n"))
}

pub fn load_live_config() -> Result<LoadedConfig, Vec<String>> {
    let path = config_path();
    if !path.exists() {
        return Ok(LoadedConfig {
            config: Config::default(),
            diagnostics: Vec::new(),
            invalid_sections: Vec::new(),
        });
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|err| vec![format!("config read error: {err}; keeping current config")])?;
    load_live_config_from_str(&content)
}

fn load_live_config_from_str(content: &str) -> Result<LoadedConfig, Vec<String>> {
    let value = content
        .parse::<toml::Value>()
        .map_err(|err| vec![format!("config parse error: {err}; keeping current config")])?;
    let table = value.as_table().ok_or_else(|| {
        vec![
            "config parse error: top-level config must be a table; keeping current config"
                .to_string(),
        ]
    })?;

    let mut config = Config::default();
    let mut diagnostics = unknown_top_level_section_diagnostics(table);
    let mut invalid_sections = Vec::new();

    if let Some(value) = table.get("onboarding") {
        match value.clone().try_into::<Option<bool>>() {
            Ok(onboarding) => config.onboarding = onboarding,
            Err(err) => diagnostics.push(format!(
                "invalid onboarding setting: {err}; keeping current onboarding state"
            )),
        }
    }

    load_live_section(
        table,
        "theme",
        "theme config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.theme = section,
    );
    load_live_section(
        table,
        "keys",
        "keybinding config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.keys = section,
    );
    load_live_section(
        table,
        "terminal",
        "terminal config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.terminal = section,
    );
    load_live_section(
        table,
        "session",
        "session config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.session = section,
    );
    load_live_section(
        table,
        "update",
        "update config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.update = section,
    );
    load_live_section(
        table,
        "ui",
        "ui config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.ui = section,
    );
    load_live_section(
        table,
        "advanced",
        "advanced config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.advanced = section,
    );
    load_live_section(
        table,
        "worktrees",
        "worktree config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.worktrees = section,
    );
    load_live_section(
        table,
        "experimental",
        "experimental config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.experimental = section,
    );
    load_live_section(
        table,
        "remote",
        "remote config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.remote = section,
    );

    Ok(LoadedConfig {
        config,
        diagnostics,
        invalid_sections,
    })
}

fn unknown_top_level_section_diagnostics_from_str(content: &str) -> Vec<String> {
    content
        .parse::<toml::Value>()
        .ok()
        .and_then(|value| value.as_table().map(unknown_top_level_section_diagnostics))
        .unwrap_or_default()
}

fn unknown_top_level_section_diagnostics(
    table: &toml::map::Map<String, toml::Value>,
) -> Vec<String> {
    table
        .iter()
        .filter_map(|(key, value)| unknown_top_level_section_diagnostic(key, value))
        .collect()
}

fn unknown_top_level_section_diagnostic(key: &str, value: &toml::Value) -> Option<String> {
    if KNOWN_TOP_LEVEL_CONFIG_KEYS.contains(&key) {
        return None;
    }

    let header = if value.is_table() {
        format!("[{key}]")
    } else if value
        .as_array()
        .is_some_and(|items| !items.is_empty() && items.iter().all(toml::Value::is_table))
    {
        format!("[[{key}]]")
    } else {
        return None;
    };

    if key == "toast" {
        Some(format!(
            "unknown config section {header}; did you mean [ui.toast]? ignoring section"
        ))
    } else {
        Some(format!("unknown config section {header}; ignoring section"))
    }
}

fn load_live_section<T>(
    table: &toml::map::Map<String, toml::Value>,
    section: &'static str,
    label: &str,
    diagnostics: &mut Vec<String>,
    invalid_sections: &mut Vec<String>,
    apply: impl FnOnce(T),
) where
    T: serde::de::DeserializeOwned,
{
    let Some(value) = table.get(section) else {
        return;
    };

    match value.clone().try_into::<T>() {
        Ok(section_config) => apply(section_config),
        Err(err) => {
            diagnostics.push(format!(
                "invalid {label}: {err}; keeping current {section} settings"
            ));
            invalid_sections.push(section.to_string());
        }
    }
}

pub(crate) fn upsert_top_level_bool(content: &str, key: &str, value: bool) -> String {
    let replacement = format!("{key} = {value}");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = replacement.clone();
            return lines.join("\n") + "\n";
        }
    }

    if lines.is_empty() {
        format!("{replacement}\n")
    } else {
        format!("{replacement}\n{}\n", lines.join("\n").trim_end())
    }
}

/// Write a key = value pair in a TOML section (creates section if missing).
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    upsert_section_raw(content, section, key, value)
}

pub fn upsert_section_bool(content: &str, section: &str, key: &str, value: bool) -> String {
    upsert_section_raw(content, section, key, &value.to_string())
}

pub fn remove_section_key(content: &str, section: &str, key: &str) -> String {
    let header = format!("[{section}]");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut in_section = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == header;
            result.push(line.to_string());
            i += 1;
            continue;
        }

        if in_section
            && (trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")))
        {
            i += 1;
            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    result.join("\n") + "\n"
}

pub fn remove_keybinding_config_sections(content: &str) -> (String, bool) {
    let mut result = Vec::new();
    let mut removed = false;
    let mut skipping_key_section = false;
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(table_name) = toml_table_header_name(trimmed) {
            in_table = true;
            skipping_key_section = is_keys_table_name(table_name);
            if skipping_key_section {
                removed = true;
                continue;
            }
        } else if skipping_key_section || (!in_table && is_top_level_keys_assignment(trimmed)) {
            removed = true;
            continue;
        }

        result.push(line.to_string());
    }

    let mut updated = result.join("\n");
    if content.ends_with('\n') || !updated.is_empty() {
        updated.push('\n');
    }
    (updated, removed)
}

// --- Remote host config mutation helpers -------------------------------------
//
// `herdr remote add`/`remove` mutate only the local config file. These helpers
// are line-preserving so user comments and unrelated sections survive, and they
// are bounded so the result always re-parses as valid TOML for the remote
// section shape. The CLI layer validates the combined host registry before and
// after applying these transforms and leaves the file unchanged on any failure.

/// Set `enabled = true` in the `[remote]` section in a line-preserving way.
///
/// Handles three shapes:
/// - `[remote]` already present: set/replace `enabled` within that section,
///   stopping at the next table header so a following `[[remote.hosts]]`
///   array-of-tables block is never re-bound to a stray `[remote]` key.
/// - `[remote]` absent but one or more `[[remote.hosts]]` present: insert a
///   `[remote]` header with `enabled = true` immediately before the first host
///   block, so the table header precedes the array-of-tables entries (required
///   for valid TOML — a `[remote]` header emitted *after* `[[remote.hosts]]`
///   would bind to the last host table, not the remote section).
/// - neither present: append a `[remote]` section at the end.
///
/// All other lines (comments, other sections, existing keys such as
/// `manage_ssh_config`) are preserved.
pub fn ensure_remote_enabled(content: &str) -> String {
    const HEADER: &str = "[remote]";
    let lines: Vec<&str> = content.lines().collect();

    // Case 1: a `[remote]` header exists (with or without a trailing inline
    // comment, e.g. `[remote] # comment`). Set `enabled = true` in place.
    if let Some(start) = lines
        .iter()
        .position(|line| toml_table_header_name(line.trim()) == Some("remote"))
    {
        return upsert_key_in_section(&lines, start, "enabled", "true");
    }

    // Case 2: no `[remote]` header, but `[[remote.hosts]]` exists. Insert a
    // `[remote]` header + `enabled = true` immediately before the first host
    // block so the table precedes the array-of-tables entries.
    if let Some(insert_at) = lines
        .iter()
        .position(|line| toml_table_header_name(line.trim()) == Some("remote.hosts"))
    {
        let mut result: Vec<String> = Vec::with_capacity(lines.len() + 3);
        for (index, line) in lines.iter().enumerate() {
            if index == insert_at {
                result.push(HEADER.to_string());
                result.push("enabled = true".to_string());
                result.push(String::new());
            }
            result.push(line.to_string());
        }
        return result.join("\n") + "\n";
    }

    // Case 3: neither present. Append a `[remote]` section at the end.
    let mut result: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
        result.push(String::new());
    }
    result.push(HEADER.to_string());
    result.push("enabled = true".to_string());
    result.join("\n") + "\n"
}

/// Append a pre-formatted `[[remote.hosts]]` block at the end of the file.
///
/// The block text is appended after existing content with a blank-line
/// separator, so the new array-of-tables entry is self-contained and the
/// result parses as valid TOML. The caller formats `block` as the table header
/// plus its key/value lines without a trailing newline. Existing content,
/// comments, and ordering are preserved.
pub fn append_remote_host_block(content: &str, block: &str) -> String {
    let trimmed = content.trim_end_matches('\n');
    if trimmed.is_empty() {
        format!("{block}\n")
    } else {
        format!("{trimmed}\n\n{block}\n")
    }
}

/// Remove the `[[remote.hosts]]` block whose `name` key equals `alias`, in a
/// line-preserving way.
///
/// Returns:
/// - `Ok(Some(updated))` when exactly one matching block was removed.
/// - `Ok(None)` when no matching block was found (the alias is not present).
/// - `Err(message)` when the shape is ambiguous or unsafe to remove without a
///   full reserialize (e.g. more than one block matches the alias). On `Err`
///   the caller must leave the config file unchanged and surface the message.
///
/// Block detection is header-based: a `[[remote.hosts]]` line starts a block
/// that extends through the following non-header lines (keys, comments, blanks)
/// until the next table header or EOF. The host `name` is read by re-parsing
/// each block's body as TOML so quoted/escaped names match reliably. Comments
/// and sections outside the removed block are preserved; comments immediately
/// preceding the removed header are left in place rather than guessed at.
pub fn remove_remote_host_block(content: &str, alias: &str) -> Result<Option<String>, String> {
    let lines: Vec<&str> = content.lines().collect();

    // Collect (header_index, body_end_index, parsed_name) spans for every host
    // block in document order.
    let mut spans: Vec<(usize, usize, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if toml_table_header_name(lines[i].trim()) == Some("remote.hosts") {
            let header = i;
            i += 1;
            while i < lines.len() && !is_toml_table_header(lines[i].trim()) {
                i += 1;
            }
            let name = parse_host_block_name(&lines[header + 1..i]);
            spans.push((header, i, name));
        } else {
            i += 1;
        }
    }

    let matching: Vec<usize> = spans
        .iter()
        .enumerate()
        .filter_map(|(idx, (_, _, name))| {
            name.as_deref()
                .is_some_and(|name| name == alias)
                .then_some(idx)
        })
        .collect();

    match matching.len() {
        0 => Ok(None),
        1 => {
            let (header, body_end, _) = spans[matching[0]];
            let mut result: Vec<String> = Vec::with_capacity(lines.len());
            for (index, line) in lines.iter().enumerate() {
                if (header..body_end).contains(&index) {
                    continue;
                }
                result.push(line.to_string());
            }
            // Drop trailing blank lines left behind by a removed trailing block
            // so the file ends cleanly; this never touches comments or keys.
            while result.last().is_some_and(|line| line.trim().is_empty()) {
                result.pop();
            }
            Ok(Some(result.join("\n") + "\n"))
        }
        _ => Err(format!(
            "could not safely remove remote host {alias}; multiple [[remote.hosts]] blocks match the alias (edit the file manually)"
        )),
    }
}

fn upsert_key_in_section(lines: &[&str], header_index: usize, key: &str, value: &str) -> String {
    let assignment = format!("{key} = {value}");
    let mut result: Vec<String> = Vec::with_capacity(lines.len() + 1);
    for line in &lines[..header_index] {
        result.push(line.to_string());
    }
    result.push(lines[header_index].to_string());

    let mut i = header_index + 1;
    let mut inserted = false;
    let mut boundary = lines.len();
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if is_toml_table_header(trimmed) {
            boundary = i;
            break;
        }
        if is_key_assignment(trimmed, key) {
            result.push(assignment.clone());
            inserted = true;
        } else {
            result.push(lines[i].to_string());
        }
        i += 1;
    }
    if !inserted {
        result.push(assignment);
    }
    for line in &lines[boundary..] {
        result.push(line.to_string());
    }
    result.join("\n") + "\n"
}

fn is_toml_table_header(trimmed: &str) -> bool {
    toml_table_header_name(trimmed).is_some()
}

fn is_key_assignment(trimmed: &str, key: &str) -> bool {
    trimmed
        .strip_prefix(key)
        .is_some_and(|rest| matches!(rest.chars().next(), Some('=' | ' ' | '\t')))
}

/// Parse a `[[remote.hosts]]` block body and return its `name` value, if any.
///
/// The body is wrapped in a synthetic `[host]` header and parsed as TOML so
/// quoted/escaped names match reliably rather than via fragile string slicing.
fn parse_host_block_name(body: &[&str]) -> Option<String> {
    let mut fragment = String::from("[host]\n");
    for line in body {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        fragment.push_str(line);
        fragment.push('\n');
    }
    let value: toml::Value = fragment.parse().ok()?;
    value
        .as_table()
        .and_then(|table| table.get("host"))
        .and_then(|host| host.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

/// Return the canonical dotted name of a TOML table or array-of-table header
/// line (already trimmed), or `None` if the line is not a header.
///
/// The matching close bracket (`]]` for `[[...]]`, `]` for `[...]`) is found
/// by scanning past quoted key segments so that a header whose quoted key
/// contains a `#`, `]`, or other special character is still recognized as a
/// section boundary. A TOML dotted key segment may be a basic string
/// (`"..."`, honoring `\` escapes) or a literal string (`'...'`), and either
/// may contain `#` — for example `"theme#dark"` is a single quoted key, so
/// `["theme#dark"]` is one table header rather than a `"["theme"` header
/// followed by a comment. A `#` that appears *after* the closing bracket is
/// still treated as a trailing inline comment, so `[theme] # comment` and
/// `[[remote.hosts]] # comment` keep working.
///
/// Whitespace inside the brackets is ignored, so `[remote]`, `[ remote ]`, and
/// `[[remote.hosts]]` all resolve to their inner dotted name (any surrounding
/// quotes from a quoted key are left intact, so callers compare against bare
/// names like `remote`/`remote.hosts` and a quoted form is treated as a
/// distinct, non-matching header). Content after the close bracket that is
/// neither whitespace nor a `#` comment means the line is not a header. An
/// empty inner name (e.g. `[]`) is treated as not a header.
fn toml_table_header_name(trimmed: &str) -> Option<&str> {
    let bytes = trimmed.as_bytes();
    let (open_len, close_token): (usize, &[u8]) = if bytes.starts_with(b"[[") {
        (2, b"]]")
    } else if bytes.first() == Some(&b'[') {
        (1, b"]")
    } else {
        return None;
    };

    // Scan from after the open bracket, tracking whether the cursor is inside a
    // basic string or a literal string, until the close token appears outside
    // any string. All delimiters are ASCII, so byte indexing never splits a
    // UTF-8 codepoint; quoted-key content (possibly Unicode) is simply skipped.
    let mut i = open_len;
    let mut in_basic = false;
    let mut in_literal = false;
    let mut close_start: Option<usize> = None;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_basic {
            if byte == b'\\' {
                // Skip the escaped character so an escaped quote does not close
                // the basic string.
                i += 2;
                continue;
            } else if byte == b'"' {
                in_basic = false;
            }
        } else if in_literal {
            if byte == b'\'' {
                in_literal = false;
            }
        } else if byte == b'"' {
            in_basic = true;
        } else if byte == b'\'' {
            in_literal = true;
        } else if bytes[i..].starts_with(close_token) {
            close_start = Some(i);
            break;
        }
        i += 1;
    }

    let close_start = close_start?;
    let inner = &trimmed[open_len..close_start];
    let trailing = trimmed[close_start + close_token.len()..].trim_start();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return None;
    }
    let name = inner.trim();
    (!name.is_empty()).then_some(name)
}

fn is_keys_table_name(name: &str) -> bool {
    name == "keys" || name.starts_with("keys.")
}

fn is_top_level_keys_assignment(trimmed: &str) -> bool {
    trimmed.starts_with("keys ") || trimmed.starts_with("keys=") || trimmed.starts_with("keys.")
}

fn upsert_section_raw(content: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let assignment = format!("{key} = {value}");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut found_section = false;
    let mut inserted = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == header {
            found_section = true;
            result.push(line.to_string());
            i += 1;

            while i < lines.len() {
                let current = lines[i];
                let current_trimmed = current.trim();
                if current_trimmed.starts_with('[') && current_trimmed.ends_with(']') {
                    if !inserted {
                        result.push(assignment.clone());
                        inserted = true;
                    }
                    break;
                }

                if current_trimmed.starts_with(&format!("{key} "))
                    || current_trimmed.starts_with(&format!("{key}="))
                {
                    result.push(assignment.clone());
                    inserted = true;
                } else {
                    result.push(current.to_string());
                }
                i += 1;
            }

            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    if !found_section {
        if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
            result.push(String::new());
        }
        result.push(header);
        result.push(assignment);
    } else if !inserted {
        result.push(assignment);
    }

    result.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_top_level_bool_replaces_existing_value() {
        let content = "onboarding = true\n[keys]\nprefix = \"ctrl+b\"\n";
        let updated = upsert_top_level_bool(content, "onboarding", false);
        assert!(updated.contains("onboarding = false"));
        assert!(!updated.contains("onboarding = true"));
    }

    #[test]
    fn upsert_section_bool_adds_missing_section() {
        let updated = upsert_section_bool("", "ui.toast", "enabled", true);
        assert!(updated.contains("[ui.toast]"));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn remove_section_key_removes_matching_key_from_section() {
        let content =
            "[ui.toast]\nenabled = true\ndelivery = \"herdr\"\n[ui.sound]\nenabled = true\n";
        let updated = remove_section_key(content, "ui.toast", "enabled");
        assert!(!updated.contains("[ui.toast]\nenabled = true"));
        assert!(updated.contains("delivery = \"herdr\""));
        assert!(updated.contains("[ui.sound]\nenabled = true"));
    }

    #[test]
    fn config_diagnostic_summary_keeps_multiple_warnings_visible() {
        let diagnostics = vec![
            "one".to_string(),
            "two".to_string(),
            "three".to_string(),
            "four".to_string(),
            "five".to_string(),
        ];

        assert_eq!(
            config_diagnostic_summary(&diagnostics).as_deref(),
            Some("one\ntwo\nthree\nfour\nand 1 more config warnings")
        );
    }

    #[test]
    fn load_live_config_parses_session_section() {
        let loaded = load_live_config_from_str(
            r#"
[session]
resume_agents_on_restore = true
"#,
        )
        .unwrap();

        assert!(loaded.config.session.resume_agents_on_restore);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_parses_remote_section() {
        let loaded = load_live_config_from_str(
            r#"
[remote]
enabled = true

[[remote.hosts]]
name = "jafar"
target = "jafar"
"#,
        )
        .unwrap();

        assert!(loaded.config.remote.enabled);
        assert_eq!(loaded.config.remote.hosts.len(), 1);
        assert_eq!(loaded.config.remote.hosts[0].name, "jafar");
        assert_eq!(
            loaded.config.remote.hosts[0].session,
            crate::session::DEFAULT_SESSION_NAME
        );
        assert!(loaded.config.remote.hosts[0]
            .connection_policy
            .starts_automatically());
        assert!(loaded.config.remote.manage_ssh_config);
        assert!(loaded.diagnostics.is_empty());
        assert!(loaded.invalid_sections.is_empty());
    }

    #[test]
    fn load_live_config_warns_about_unknown_top_level_sections() {
        let loaded = load_live_config_from_str(
            r#"
[toast]
delivery = "system"

[ui.toast]
delivery = "herdr"
"#,
        )
        .unwrap();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [toast]; did you mean [ui.toast]? ignoring section"]
        );
        assert!(loaded.invalid_sections.is_empty());
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
    }

    #[test]
    fn load_live_config_does_not_warn_about_unknown_top_level_scalar_values() {
        let loaded = load_live_config_from_str(
            r#"
plugin = []

[ui.toast]
delivery = "herdr"
"#,
        )
        .unwrap();

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::Herdr
        );
    }

    #[test]
    fn startup_config_load_warns_about_unknown_top_level_sections() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-config-unknown-section-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[[plugin]]
id = "example"

[ui.toast]
delivery = "system"
"#,
        )
        .unwrap();
        std::env::set_var(CONFIG_PATH_ENV_VAR, &path);

        let loaded = Config::load();

        assert_eq!(
            loaded.diagnostics,
            vec!["unknown config section [[plugin]]; ignoring section"]
        );
        assert_eq!(
            loaded.config.ui.toast.delivery,
            super::super::ToastDelivery::System
        );

        std::env::remove_var(CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remove_keybinding_config_sections_removes_keys_tables_only() {
        let content = r#"onboarding = false

[theme]
name = "catppuccin"

[keys]
prefix = "ctrl+a"
new_tab = "c"

[[keys.command]]
key = "g"
command = "lazygit"

[keys.indexed]
tabs = "ctrl"

[ui]
mouse_capture = false
"#;

        let (updated, removed) = remove_keybinding_config_sections(content);

        assert!(removed);
        assert!(updated.contains("onboarding = false"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("[ui]\nmouse_capture = false"));
        assert!(!updated.contains("[keys]"));
        assert!(!updated.contains("[[keys.command]]"));
        assert!(!updated.contains("[keys.indexed]"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_keybinding_config_sections_reports_noop_without_keys() {
        let content = "[ui]\nmouse_capture = true\n";
        let (updated, removed) = remove_keybinding_config_sections(content);
        assert!(!removed);
        assert_eq!(updated, content);
    }

    #[test]
    fn ensure_remote_enabled_appends_section_when_absent() {
        let updated = ensure_remote_enabled("");
        assert_eq!(updated, "[remote]\nenabled = true\n");
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn ensure_remote_enabled_flips_existing_false_in_place() {
        let content = "[remote]\nenabled = false\nmanage_ssh_config = true\n";
        let updated = ensure_remote_enabled(content);
        // The flip happens within the existing [remote] section, preserving
        // manage_ssh_config and the existing section order.
        assert!(updated.contains("[remote]\nenabled = true\nmanage_ssh_config = true"));
        assert!(!updated.contains("enabled = false"));
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(value["remote"]["enabled"].as_bool(), Some(true));
        assert_eq!(value["remote"]["manage_ssh_config"].as_bool(), Some(true));
    }

    #[test]
    fn ensure_remote_enabled_inserts_key_into_section_missing_enabled() {
        // Reviewer A case (a): [remote] exists with manage_ssh_config but no
        // enabled key. The key must be added inside the section, not appended
        // after any following [[remote.hosts]].
        let content = "[remote]\nmanage_ssh_config = true\n";
        let updated = ensure_remote_enabled(content);
        assert!(updated.contains("[remote]\nmanage_ssh_config = true\nenabled = true"));
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(value["remote"]["enabled"].as_bool(), Some(true));
        assert_eq!(value["remote"]["manage_ssh_config"].as_bool(), Some(true));
    }

    #[test]
    fn ensure_remote_enabled_inserts_header_before_existing_hosts() {
        // Dangerous shape: no [remote] header, but [[remote.hosts]] present. A
        // naive append would emit [remote] *after* the array-of-tables and bind
        // it to the last host table. The header must be inserted before the
        // first host block.
        let content =
            "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        let updated = ensure_remote_enabled(content);
        assert!(updated.starts_with("[remote]\nenabled = true\n\n[[remote.hosts]]"));
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(value["remote"]["enabled"].as_bool(), Some(true));
        assert_eq!(value["remote"]["hosts"][0]["name"].as_str(), Some("jafar"));
    }

    #[test]
    fn ensure_remote_enabled_preserves_other_sections_and_comments() {
        let content =
            "# top comment\n[theme]\nname = \"catppuccin\"\n\n[remote]\nenabled = false\n";
        let updated = ensure_remote_enabled(content);
        assert!(updated.contains("# top comment"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn ensure_remote_enabled_treats_trailing_comment_header_as_boundary() {
        // Reviewer B case (a): a valid TOML table header with a trailing inline
        // comment (`[theme] # ...`) must be detected as a section boundary so
        // `enabled` is set inside [remote], not on the key that follows the
        // theme header. The theme table's `enabled = false` must be preserved.
        let content =
            "[remote]\n# maybe existing config\n\n[theme] # inline table comment\nenabled = false\n";
        let updated = ensure_remote_enabled(content);
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(
            value["remote"]["enabled"].as_bool(),
            Some(true),
            "remote.enabled must be true; got:\n{updated}"
        );
        assert_eq!(
            value["theme"]["enabled"].as_bool(),
            Some(false),
            "theme.enabled must stay false; got:\n{updated}"
        );
        assert!(updated.contains("[theme] # inline table comment"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn ensure_remote_enabled_preserves_quoted_table_with_hash_in_key() {
        // Reviewer B quoted-header case: a valid TOML table header whose quoted
        // key contains `#` (e.g. `["theme#dark"]`) must be detected as a section
        // boundary. With the old first-`#`-is-a-comment heuristic the header was
        // not recognized, so `ensure_remote_enabled` walked past it and rewrote
        // the unrelated quoted table's `enabled = false` to true. Here the
        // `[remote]` section is already enabled, so the only acceptable change
        // is a no-op: remote.enabled stays true and theme#dark stays false.
        let content = "[remote]\nenabled = true\n\n[\"theme#dark\"]\nenabled = false\n";
        let updated = ensure_remote_enabled(content);
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(
            value["remote"]["enabled"].as_bool(),
            Some(true),
            "remote.enabled must be true; got:\n{updated}"
        );
        assert_eq!(
            value["theme#dark"]["enabled"].as_bool(),
            Some(false),
            "theme#dark.enabled must stay false; got:\n{updated}"
        );
        assert!(updated.contains("[\"theme#dark\"]"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn ensure_remote_enabled_inserts_key_before_quoted_table_with_hash_in_key() {
        // Same boundary detection, but `[remote]` lacks an `enabled` key so it
        // must be inserted inside the section and stop at the quoted header.
        let content = "[remote]\nmanage_ssh_config = true\n\n[\"theme#dark\"]\nenabled = false\n";
        let updated = ensure_remote_enabled(content);
        let value: toml::Value = toml::from_str(&updated).unwrap();
        assert_eq!(value["remote"]["enabled"].as_bool(), Some(true));
        assert_eq!(value["remote"]["manage_ssh_config"].as_bool(), Some(true));
        assert_eq!(
            value["theme#dark"]["enabled"].as_bool(),
            Some(false),
            "theme#dark.enabled must stay false; got:\n{updated}"
        );
        assert!(updated.contains("[\"theme#dark\"]"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn append_remote_host_block_adds_separator_and_trailing_newline() {
        let content = "[remote]\nenabled = true\n";
        let block = "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\nconnection_policy = \"auto\"\nconnect_timeout_secs = 10";
        let updated = append_remote_host_block(content, block);
        assert!(updated.contains("[remote]\nenabled = true\n\n[[remote.hosts]]"));
        assert!(updated.ends_with("connect_timeout_secs = 10\n"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn append_remote_host_block_on_empty_content() {
        let block = "[[remote.hosts]]\nname = \"a\"\ntarget = \"a\"\nsession = \"default\"";
        let updated = append_remote_host_block("", block);
        assert_eq!(
            updated,
            "[[remote.hosts]]\nname = \"a\"\ntarget = \"a\"\nsession = \"default\"\n"
        );
    }

    #[test]
    fn remove_remote_host_block_returns_none_for_unknown_alias() {
        let content = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        assert_eq!(remove_remote_host_block(content, "missing").unwrap(), None);
        // No change to content on the None path.
        assert_eq!(remove_remote_host_block(content, "missing").unwrap(), None);
    }

    #[test]
    fn remove_remote_host_block_removes_one_of_multiple_hosts() {
        let content = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n\n[[remote.hosts]]\nname = \"work\"\ntarget = \"work\"\nsession = \"agents\"\n";
        let updated = remove_remote_host_block(content, "jafar").unwrap().unwrap();
        assert!(!updated.contains("name = \"jafar\""));
        assert!(updated.contains("name = \"work\""));
        assert!(updated.contains("[remote]\nenabled = true"));
        let value: toml::Value = toml::from_str(&updated).unwrap();
        let hosts = value["remote"]["hosts"].as_array().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0]["name"].as_str(), Some("work"));
    }

    #[test]
    fn remove_remote_host_block_preserves_unrelated_comments_and_sections() {
        let content = "# keep this\n[theme]\nname = \"catppuccin\"\n\n# jafar entry\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n\n[[remote.hosts]]\nname = \"work\"\ntarget = \"work\"\nsession = \"agents\"\n";
        let updated = remove_remote_host_block(content, "jafar").unwrap().unwrap();
        assert!(updated.contains("# keep this"));
        assert!(updated.contains("[theme]\nname = \"catppuccin\""));
        assert!(updated.contains("name = \"work\""));
        assert!(!updated.contains("name = \"jafar\""));
        // The comment immediately preceding the removed header is left in place
        // rather than guessed at; the section/comment after it is untouched.
        assert!(updated.contains("# jafar entry"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_remote_host_block_errors_on_ambiguous_duplicate_alias() {
        // Registry validation rejects duplicates, but the line-preserving
        // helper must still fail safe rather than guessing which block to drop.
        let content = "[[remote.hosts]]\nname = \"dup\"\ntarget = \"a\"\nsession = \"default\"\n\n[[remote.hosts]]\nname = \"dup\"\ntarget = \"b\"\nsession = \"default\"\n";
        let result = remove_remote_host_block(content, "dup");
        assert!(result.is_err());
    }

    #[test]
    fn remove_remote_host_block_trims_trailing_blank_from_removed_last_block() {
        let content = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        let updated = remove_remote_host_block(content, "jafar").unwrap().unwrap();
        assert!(!updated.contains("[[remote.hosts]]"));
        assert!(updated.contains("[remote]\nenabled = true"));
        // Ends cleanly without a dangling blank line.
        assert!(!updated.ends_with("\n\n"));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_remote_host_block_treats_trailing_comment_header_as_boundary() {
        // Reviewer B case (b): a [[remote.hosts]] block followed by an
        // unrelated table header with a trailing inline comment must remove
        // ONLY the host block, preserving the theme section and its
        // `enabled = false` key. Previously the `[theme] # ...` line was not
        // detected as a header, so the whole trailing section was deleted.
        let content = "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\nconnection_policy = \"auto\"\nconnect_timeout_secs = 10\n\n[theme] # inline table comment\nenabled = false\n";
        let updated = remove_remote_host_block(content, "jafar").unwrap().unwrap();
        let value: toml::Value = toml::from_str(&updated).unwrap();
        // No remote hosts remain.
        let hosts_empty = value
            .get("remote")
            .and_then(|remote| remote.get("hosts"))
            .and_then(|hosts| hosts.as_array())
            .map(|array| array.is_empty())
            .unwrap_or(true);
        assert!(
            hosts_empty,
            "no remote hosts should remain; got:\n{updated}"
        );
        // Theme section and its enabled key are preserved verbatim.
        assert_eq!(
            value["theme"]["enabled"].as_bool(),
            Some(false),
            "theme.enabled must be preserved; got:\n{updated}"
        );
        assert!(updated.contains("[theme] # inline table comment"));
        assert!(!updated.contains("name = \"jafar\""));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn remove_remote_host_block_preserves_quoted_table_with_hash_in_key() {
        // Reviewer B quoted-header case: removing a `[[remote.hosts]]` block
        // followed by a quoted table whose key contains `#` must delete only the
        // host block and preserve the quoted table and its `enabled = false`.
        // With the old first-`#` heuristic the quoted header was not seen as a
        // boundary, so the block extended through theme#dark and deleted it.
        let content = "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\nconnection_policy = \"auto\"\nconnect_timeout_secs = 10\n\n[\"theme#dark\"]\nenabled = false\n";
        let updated = remove_remote_host_block(content, "jafar").unwrap().unwrap();
        let value: toml::Value = toml::from_str(&updated).unwrap();
        let hosts_empty = value
            .get("remote")
            .and_then(|remote| remote.get("hosts"))
            .and_then(|hosts| hosts.as_array())
            .map(|array| array.is_empty())
            .unwrap_or(true);
        assert!(
            hosts_empty,
            "no remote hosts should remain; got:\n{updated}"
        );
        assert_eq!(
            value["theme#dark"]["enabled"].as_bool(),
            Some(false),
            "theme#dark section must be preserved; got:\n{updated}"
        );
        assert!(updated.contains("[\"theme#dark\"]"));
        assert!(!updated.contains("name = \"jafar\""));
        assert!(toml::from_str::<toml::Value>(&updated).is_ok());
    }

    #[test]
    fn full_add_round_trip_validates_combined_registry() {
        // Reviewer A cases (a)-(e): add a first host when [remote] already has
        // manage_ssh_config; flip enabled; two sequential adds; comments
        // preserved; re-load through Config + RemoteHostRegistry validates.
        use crate::remote_target::{RemoteConnectionPolicy, RemoteHostConfig, RemoteHostRegistry};

        fn block(host: &RemoteHostConfig) -> String {
            format!(
                "[[remote.hosts]]\nname = \"{}\"\ntarget = \"{}\"\nsession = \"{}\"\nconnection_policy = \"{}\"\nconnect_timeout_secs = {}",
                host.name, host.target, host.session, host.connection_policy.as_toml_str(), host.connect_timeout_secs
            )
        }

        fn round_trip(content: &str, host: RemoteHostConfig) -> String {
            let existing: crate::config::Config = toml::from_str(content).unwrap();
            let mut combined = existing.remote.hosts;
            combined.push(host.clone());
            RemoteHostRegistry::from_configs(combined).unwrap();
            let enabled = ensure_remote_enabled(content);
            append_remote_host_block(&enabled, &block(&host))
        }

        // (a) [remote] already exists with manage_ssh_config set, add first host.
        let base = "# user comment\n[remote]\nmanage_ssh_config = true\n";
        let first = round_trip(
            base,
            RemoteHostConfig::from_explicit_fields(
                "jafar",
                "jafar",
                "default",
                RemoteConnectionPolicy::Auto,
                10,
            ),
        );
        let parsed: crate::config::Config = toml::from_str(&first).unwrap();
        assert!(parsed.remote.enabled);
        assert!(parsed.remote.manage_ssh_config);
        assert_eq!(parsed.remote.hosts.len(), 1);
        assert_eq!(parsed.remote.hosts[0].name, "jafar");
        assert!(first.contains("# user comment"));
        RemoteHostRegistry::from_configs(parsed.remote.hosts).unwrap();

        // (b) enabled = false present must flip to true and still parse.
        let with_false = ensure_remote_enabled("[remote]\nenabled = false\n");
        let parsed: crate::config::Config = toml::from_str(&with_false).unwrap();
        assert!(parsed.remote.enabled);

        // (c) two sequential adds produce two valid [[remote.hosts]].
        let after_second = round_trip(
            &first,
            RemoteHostConfig::from_explicit_fields(
                "work",
                "work",
                "agents",
                RemoteConnectionPolicy::OnDemand,
                20,
            ),
        );
        let parsed: crate::config::Config = toml::from_str(&after_second).unwrap();
        assert_eq!(parsed.remote.hosts.len(), 2);
        assert_eq!(parsed.remote.hosts[1].name, "work");
        RemoteHostRegistry::from_configs(parsed.remote.hosts).unwrap();

        // (d) comments preserved through both adds.
        assert!(after_second.contains("# user comment"));
    }
}
