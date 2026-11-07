use json_compilation_db::Entry;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Path aliases to canonical paths, and canonical paths to original entry paths.
/// Ambiguous aliases are omitted from the forward mapping.
pub type PathMappings = (HashMap<PathBuf, PathBuf>, HashMap<PathBuf, PathBuf>);

#[derive(Error, Debug)]
pub enum CompilationDatabaseError {
    #[error("Compilation database file not found: {path}")]
    FileNotFound { path: String },
    #[error("Failed to read compilation database file: {error}")]
    ReadError { error: String },
    #[error("Failed to parse compilation database JSON: {error}")]
    ParseError { error: String },
    #[error("Compilation database is empty")]
    EmptyDatabase,
}

/// Wrapper around compilation database providing structured access to compilation entries
///
/// This struct contains both the path to the compilation database file and the parsed entries.
/// When serialized, only the path is included in the output to avoid serializing large database content.
#[derive(Debug, Clone, Deserialize)]
pub struct CompilationDatabase {
    /// Path to the compilation database file (compile_commands.json)
    pub path: PathBuf,
    /// Parsed compilation database entries (loaded at initialization)
    #[serde(skip)]
    pub entries: Vec<Entry>,
}

impl CompilationDatabase {
    /// Create a new compilation database by loading and parsing the file at the given path
    ///
    /// This immediately loads and parses the compilation database, returning an error if
    /// the file doesn't exist, can't be read, or contains invalid JSON.
    pub fn new(path: PathBuf) -> Result<Self, CompilationDatabaseError> {
        // Check if file exists
        if !path.exists() {
            return Err(CompilationDatabaseError::FileNotFound {
                path: path.to_string_lossy().to_string(),
            });
        }

        // Open and read the file
        let file = std::fs::File::open(&path).map_err(|e| CompilationDatabaseError::ReadError {
            error: e.to_string(),
        })?;

        // Parse the JSON compilation database
        let reader = std::io::BufReader::new(file);
        let entries: Vec<Entry> =
            serde_json::from_reader(reader).map_err(|e| CompilationDatabaseError::ParseError {
                error: e.to_string(),
            })?;

        // Check if database is empty
        if entries.is_empty() {
            return Err(CompilationDatabaseError::EmptyDatabase);
        }

        Ok(Self { path, entries })
    }

    /// Create a compilation database from entries for testing
    ///
    /// This bypasses filesystem operations and creates a CompilationDatabase
    /// directly from provided entries, useful for unit tests.
    #[cfg(test)]
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        Self {
            path: PathBuf::from("/test/compile_commands.json"),
            entries,
        }
    }

    /// Get all compilation database entries
    #[allow(dead_code)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Get the path to the compilation database file
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Get all unique source files with canonicalized paths
    ///
    /// Relative file paths are resolved against each entry's working directory.
    /// A relative working directory is resolved against the compilation database directory.
    /// Existing files are canonicalized; missing files retain their resolved paths.
    pub fn canonical_source_files(&self) -> Result<Vec<PathBuf>, CompilationDatabaseError> {
        let mut canonical_files = Vec::new();
        let mut seen_files = HashSet::new();

        for entry in &self.entries {
            let canonical_path = self.canonicalize_entry_path(entry)?;
            if seen_files.insert(canonical_path.clone()) {
                canonical_files.push(canonical_path);
            }
        }

        canonical_files.sort();
        Ok(canonical_files)
    }

    /// Get mappings between entry path aliases and canonical paths.
    ///
    /// Forward mappings include original and working-directory-resolved paths. An original
    /// spelling shared by different files is omitted rather than selecting an arbitrary entry.
    /// Reverse mappings preserve the original file spelling from the compilation database.
    pub fn path_mappings(&self) -> Result<PathMappings, CompilationDatabaseError> {
        let mut original_to_canonical = HashMap::new();
        let mut canonical_to_original = HashMap::new();
        let mut ambiguous_paths = HashSet::new();

        for entry in &self.entries {
            let canonical_path = self.canonicalize_entry_path(entry)?;
            let resolved_path = self.resolve_entry_path(entry)?;

            for alias in [entry.file.clone(), resolved_path] {
                if ambiguous_paths.contains(&alias) {
                    continue;
                }
                if let Some(previous) = original_to_canonical.get(&alias)
                    && previous != &canonical_path
                {
                    original_to_canonical.remove(&alias);
                    ambiguous_paths.insert(alias);
                } else {
                    original_to_canonical.insert(alias, canonical_path.clone());
                }
            }
            canonical_to_original.insert(canonical_path, entry.file.clone());
        }

        Ok((original_to_canonical, canonical_to_original))
    }

    /// Resolve file paths using compilation database working-directory semantics.
    fn resolve_entry_path(&self, entry: &Entry) -> Result<PathBuf, CompilationDatabaseError> {
        if entry.file.is_absolute() {
            return Ok(entry.file.clone());
        }

        let working_directory = if entry.directory.is_absolute() {
            entry.directory.clone()
        } else {
            // Relative (or empty) working directories use the database's directory as a base.
            let database_directory = self.path.parent().unwrap_or_else(|| Path::new("."));
            let absolute_database_directory =
                std::path::absolute(database_directory).map_err(|e| {
                    CompilationDatabaseError::ReadError {
                        error: format!("Failed to resolve compilation database directory: {e}"),
                    }
                })?;
            absolute_database_directory.join(&entry.directory)
        };

        Ok(working_directory.join(&entry.file))
    }

    fn canonicalize_entry_path(&self, entry: &Entry) -> Result<PathBuf, CompilationDatabaseError> {
        let resolved_path = self.resolve_entry_path(entry)?;
        Ok(resolved_path.canonicalize().unwrap_or(resolved_path))
    }
}

/// Custom serialization that only outputs the path field
///
/// This ensures that when the CompilationDatabase is serialized (e.g., in JSON responses),
/// only the path is included, not the potentially large entries array.
impl Serialize for CompilationDatabase {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.path.serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn entry(directory: impl Into<PathBuf>, file: impl Into<PathBuf>) -> Entry {
        Entry {
            directory: directory.into(),
            file: file.into(),
            arguments: vec!["c++".to_string(), "-c".to_string()],
            output: None,
        }
    }

    fn database(root: &Path, entries: Vec<Entry>) -> CompilationDatabase {
        CompilationDatabase {
            path: root.join("compile_commands.json"),
            entries,
        }
    }

    #[test]
    fn relative_files_use_entry_working_directory() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("project with spaces/source file.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "").unwrap();
        let db = database(
            &temp.path().join("unrelated database directory"),
            vec![entry(source.parent().unwrap(), "source file.cpp")],
        );
        let canonical = source.canonicalize().unwrap();

        assert_eq!(
            db.canonical_source_files().unwrap(),
            vec![canonical.clone()]
        );
        let (forward, reverse) = db.path_mappings().unwrap();
        assert_eq!(forward.get(Path::new("source file.cpp")), Some(&canonical));
        assert_eq!(forward.get(&source), Some(&canonical));
        assert_eq!(
            reverse.get(&canonical),
            Some(&PathBuf::from("source file.cpp"))
        );
    }

    #[test]
    fn absolute_files_ignore_entry_working_directory() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.cpp");
        std::fs::write(&source, "").unwrap();
        let db = database(
            &temp.path().join("database"),
            vec![entry("missing working directory", &source)],
        );

        assert_eq!(
            db.canonical_source_files().unwrap(),
            vec![source.canonicalize().unwrap()]
        );
    }

    #[test]
    fn relative_and_empty_working_directories_use_database_directory() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source directory/source.cpp");
        let local_source = temp.path().join("local.cpp");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(&local_source, "").unwrap();
        std::fs::create_dir(temp.path().join("build")).unwrap();
        let db = database(
            temp.path(),
            vec![
                entry("source directory", "source.cpp"),
                entry("", "local.cpp"),
                entry("build", "../source directory/source.cpp"),
            ],
        );
        let mut expected = vec![
            source.canonicalize().unwrap(),
            local_source.canonicalize().unwrap(),
        ];
        expected.sort();

        assert_eq!(db.canonical_source_files().unwrap(), expected);
    }

    #[test]
    fn duplicate_relative_spellings_do_not_select_an_arbitrary_file() {
        let temp = TempDir::new().unwrap();
        let first_directory = temp.path().join("first project");
        let second_directory = temp.path().join("second project");
        for directory in [&first_directory, &second_directory] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("main.cpp"), "").unwrap();
        }
        let db = database(
            temp.path(),
            vec![
                entry(&first_directory, "main.cpp"),
                entry(&second_directory, "main.cpp"),
                entry(&first_directory, "main.cpp"),
            ],
        );
        let first = first_directory.join("main.cpp").canonicalize().unwrap();
        let second = second_directory.join("main.cpp").canonicalize().unwrap();
        let mut expected = vec![first.clone(), second.clone()];
        expected.sort();

        assert_eq!(db.canonical_source_files().unwrap(), expected);
        let (forward, reverse) = db.path_mappings().unwrap();
        assert!(!forward.contains_key(Path::new("main.cpp")));
        assert_eq!(forward.get(&first_directory.join("main.cpp")), Some(&first));
        assert_eq!(
            forward.get(&second_directory.join("main.cpp")),
            Some(&second)
        );
        assert_eq!(reverse.len(), 2);
    }

    #[test]
    fn missing_files_retain_resolved_working_directory_paths() {
        let temp = TempDir::new().unwrap();
        let db = database(
            temp.path(),
            vec![entry("generated directory", "generated.cpp")],
        );
        let expected = temp.path().join("generated directory/generated.cpp");

        assert_eq!(db.canonical_source_files().unwrap(), vec![expected.clone()]);
        assert_eq!(
            db.path_mappings()
                .unwrap()
                .0
                .get(Path::new("generated.cpp")),
            Some(&expected)
        );
    }
}
