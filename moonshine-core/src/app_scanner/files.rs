use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::desktop::SUPPORTED_IMAGE_EXTENSIONS;
use crate::session::application::ApplicationConfig;

fn default_true() -> bool {
	true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilesApplicationScannerConfig {
	/// Directories to scan for game files.
	pub directories: Vec<PathBuf>,

	/// File extensions to match, without the leading dot (eg. `["iso"]`). Matching is case-insensitive.
	pub extensions: Vec<String>,

	/// The command to run for each file.
	///
	/// `{path}` is replaced with the absolute path of the file, `{file_name}` with its file name
	/// and `{title}` with the derived application title.
	pub command: Vec<String>,

	/// Whether to scan subdirectories.
	#[serde(default = "default_true")]
	pub recursive: bool,

	/// Whether to look for box art next to each file (same file name with an image extension,
	/// or in a `boxart/` or `covers/` subdirectory).
	#[serde(default = "default_true")]
	pub resolve_boxart: bool,

	/// Commands to run before launching each scanned application.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pre_command: Vec<Vec<String>>,

	/// Commands to run after each scanned application's session ends.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub post_command: Vec<Vec<String>>,

	/// systemd StandardOutput value for launched applications.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stdout: Option<String>,

	/// systemd StandardError value for launched applications.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stderr: Option<String>,

	/// Seconds to wait for each scanned application to reach an active state after launch.
	#[serde(default = "crate::session::application::default_launch_timeout")]
	pub launch_timeout_secs: u64,
}

pub(crate) fn scan_files_applications(config: &FilesApplicationScannerConfig) -> Result<Vec<ApplicationConfig>, ()> {
	if config.extensions.is_empty() {
		tracing::warn!("Files application scanner has no extensions configured, skipping.");
		return Err(());
	}

	if config.command.is_empty() {
		tracing::warn!("Files application scanner has no command configured, skipping.");
		return Err(());
	}

	let extensions: Vec<&str> = config
		.extensions
		.iter()
		.map(|extension| extension.trim().trim_start_matches('.'))
		.filter(|extension| !extension.is_empty())
		.collect();

	let mut files = Vec::new();
	let mut seen_paths = HashSet::new();

	for directory in &config.directories {
		let directory = match shellexpand::full(&directory.to_string_lossy()) {
			Ok(expanded) => PathBuf::from(expanded.as_ref()),
			Err(e) => {
				tracing::warn!(
					"Failed to expand files scanner directory '{}': {e}",
					directory.display()
				);
				continue;
			},
		};

		if !directory.exists() {
			tracing::debug!("Skipping missing files scanner directory '{}'.", directory.display());
			continue;
		}

		let mut walker = WalkDir::new(&directory).follow_links(true);
		if !config.recursive {
			walker = walker.max_depth(1);
		}

		for entry in walker
			.into_iter()
			.filter_map(|entry| entry.ok())
			.filter(|entry| entry.file_type().is_file())
		{
			let matches = entry
				.path()
				.extension()
				.and_then(|extension| extension.to_str())
				.is_some_and(|extension| extensions.iter().any(|wanted| wanted.eq_ignore_ascii_case(extension)));

			if matches && seen_paths.insert(entry.path().to_path_buf()) {
				files.push(entry.into_path());
			}
		}
	}

	// Keep the client list order stable regardless of directory iteration order.
	files.sort_by_cached_key(|path| (file_stem(path).to_ascii_lowercase(), path.clone()));

	let mut applications = Vec::new();
	let mut used_titles = HashSet::new();

	for path in files {
		let stem = file_stem(&path);
		let mut title = clean_title(&stem);
		if !used_titles.insert(title.to_ascii_lowercase()) {
			// Titles determine the application ID, so they must be unique.
			title = stem.clone();
			if !used_titles.insert(title.to_ascii_lowercase()) {
				tracing::debug!(
					"Skipping '{}': an application with the same title exists.",
					path.display()
				);
				continue;
			}
		}

		let file_name = path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.unwrap_or_default();
		let path_string = path.to_string_lossy();

		let command = config
			.command
			.iter()
			.map(|argument| {
				argument
					.replace("{path}", &path_string)
					.replace("{file_name}", &file_name)
					.replace("{title}", &title)
			})
			.collect();

		let boxart = if config.resolve_boxart {
			find_boxart(&path)
		} else {
			None
		};

		applications.push(ApplicationConfig {
			title,
			boxart,
			command,
			pre_command: config.pre_command.clone(),
			post_command: config.post_command.clone(),
			stdout: config.stdout.clone(),
			stderr: config.stderr.clone(),
			launch_timeout_secs: config.launch_timeout_secs,
		});
	}

	tracing::debug!("Scanned {} game files.", applications.len());
	Ok(applications)
}

fn file_stem(path: &Path) -> String {
	path.file_stem()
		.map(|stem| stem.to_string_lossy().into_owned())
		.unwrap_or_default()
}

/// Strip trailing `(...)` and `[...]` tags, eg. `Pokemon Crystal (USA, Europe) [!]` -> `Pokemon Crystal`.
fn clean_title(stem: &str) -> String {
	let mut title = stem.trim();

	loop {
		let closing = match title.chars().last() {
			Some(')') => '(',
			Some(']') => '[',
			_ => break,
		};

		let Some(start) = title.rfind(closing) else {
			break;
		};

		title = title[..start].trim_end();
	}

	if title.is_empty() {
		stem.trim().to_string()
	} else {
		title.to_string()
	}
}

fn find_boxart(path: &Path) -> Option<PathBuf> {
	let parent = path.parent()?;
	let stem = path.file_stem()?.to_string_lossy();

	for directory in [parent.to_path_buf(), parent.join("boxart"), parent.join("covers")] {
		for extension in SUPPORTED_IMAGE_EXTENSIONS {
			let candidate = directory.join(format!("{stem}.{extension}"));
			if candidate.is_file() {
				return Some(candidate);
			}
		}
	}

	None
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tempfile::tempdir;

	use super::*;

	fn touch(path: &Path) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, "").unwrap();
	}

	fn config(directories: Vec<PathBuf>, extensions: &[&str]) -> FilesApplicationScannerConfig {
		FilesApplicationScannerConfig {
			directories,
			extensions: extensions.iter().map(|e| e.to_string()).collect(),
			command: vec!["emu".into(), "--".into(), "{path}".into()],
			recursive: true,
			resolve_boxart: true,
			pre_command: vec![],
			post_command: vec![],
			stdout: None,
			stderr: None,
			launch_timeout_secs: 10,
		}
	}

	#[test]
	fn matches_only_configured_extensions_case_insensitively() {
		let dir = tempdir().unwrap();
		touch(&dir.path().join("a.iso"));
		touch(&dir.path().join("b.ISO"));
		touch(&dir.path().join("c.bin"));

		let apps = scan_files_applications(&config(vec![dir.path().into()], &[".iso"])).unwrap();
		let titles: Vec<_> = apps.iter().map(|a| a.title.as_str()).collect();
		assert_eq!(titles, vec!["a", "b"]);
	}

	#[test]
	fn respects_recursive_flag() {
		let dir = tempdir().unwrap();
		touch(&dir.path().join("top.gbc"));
		touch(&dir.path().join("sub/nested.gbc"));

		let mut cfg = config(vec![dir.path().into()], &["gbc"]);
		assert_eq!(scan_files_applications(&cfg).unwrap().len(), 2);

		cfg.recursive = false;
		let apps = scan_files_applications(&cfg).unwrap();
		assert_eq!(apps.len(), 1);
		assert_eq!(apps[0].title, "top");
	}

	#[test]
	fn substitutes_placeholders_without_splitting_paths() {
		let dir = tempdir().unwrap();
		let file = dir.path().join("My Game (USA).iso");
		touch(&file);

		let mut cfg = config(vec![dir.path().into()], &["iso"]);
		cfg.command = vec!["emu".into(), "{path}".into(), "{file_name}".into(), "{title}".into()];

		let apps = scan_files_applications(&cfg).unwrap();
		assert_eq!(apps.len(), 1);
		assert_eq!(apps[0].title, "My Game");
		assert_eq!(
			apps[0].command,
			vec![
				"emu".to_string(),
				file.to_string_lossy().into_owned(),
				"My Game (USA).iso".to_string(),
				"My Game".to_string(),
			]
		);
	}

	#[test]
	fn cleans_titles() {
		assert_eq!(clean_title("Pokemon Crystal (USA, Europe) [!]"), "Pokemon Crystal");
		assert_eq!(clean_title("Plain"), "Plain");
		assert_eq!(clean_title("(Only Tags)"), "(Only Tags)");
	}

	#[test]
	fn colliding_titles_fall_back_to_file_stem() {
		let dir = tempdir().unwrap();
		touch(&dir.path().join("Game (USA).iso"));
		touch(&dir.path().join("Game (Europe).iso"));

		let apps = scan_files_applications(&config(vec![dir.path().into()], &["iso"])).unwrap();
		let mut titles: Vec<_> = apps.iter().map(|a| a.title.clone()).collect();
		titles.sort();
		assert_eq!(titles, vec!["Game", "Game (USA)"]);
	}

	#[test]
	fn finds_sibling_and_subdirectory_boxart() {
		let dir = tempdir().unwrap();
		touch(&dir.path().join("a.iso"));
		touch(&dir.path().join("a.png"));
		touch(&dir.path().join("b.iso"));
		touch(&dir.path().join("covers/b.jpg"));
		touch(&dir.path().join("c.iso"));

		let apps = scan_files_applications(&config(vec![dir.path().into()], &["iso"])).unwrap();
		assert_eq!(apps[0].boxart.as_deref(), Some(dir.path().join("a.png").as_path()));
		assert_eq!(
			apps[1].boxart.as_deref(),
			Some(dir.path().join("covers/b.jpg").as_path())
		);
		assert_eq!(apps[2].boxart, None);

		let mut cfg = config(vec![dir.path().into()], &["iso"]);
		cfg.resolve_boxart = false;
		assert!(
			scan_files_applications(&cfg)
				.unwrap()
				.iter()
				.all(|a| a.boxart.is_none())
		);
	}

	#[test]
	fn handles_missing_directory_and_invalid_config() {
		let dir = tempdir().unwrap();
		let missing = dir.path().join("missing");
		assert!(
			scan_files_applications(&config(vec![missing], &["iso"]))
				.unwrap()
				.is_empty()
		);

		assert!(scan_files_applications(&config(vec![dir.path().into()], &[])).is_err());

		let mut cfg = config(vec![dir.path().into()], &["iso"]);
		cfg.command.clear();
		assert!(scan_files_applications(&cfg).is_err());
	}
}
