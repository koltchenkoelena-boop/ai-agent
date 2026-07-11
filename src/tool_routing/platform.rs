// ---------------------------------------------------------------------------
// Platform (Built-in) tools
//
// Четыре нативных инструмента для работы с файловой системой:
//   read_file   — чтение файла
//   write_file  — запись файла
//   glob        — поиск файлов по glob-паттерну
//   grep        — поиск текста в файлах
//
// Бизнес-логика вынесена в отдельные async-функции (do_read_file и т.д.),
// чтобы их можно было легко заменить реальной реализацией позже.
// Сейчас функции содержат todo!() — заглушки для субагента.
// ---------------------------------------------------------------------------

use async_trait::async_trait;

use crate::tool_routing::{AsyncTool, ToolKind, ToolRouter};
use crate::types::ToolDefinition;

// ===========================================================================
// ReadFileTool
// ===========================================================================

pub struct ReadFileTool;

#[async_trait]
impl AsyncTool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file's content as UTF-8 text.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("invalid arguments for read_file: {e}"))?;
        let path = args["path"]
            .as_str()
            .ok_or_else(|| "missing required field 'path' in read_file".to_string())?;
        do_read_file(path).await.map_err(|e| e.to_string())
    }
}

// ===========================================================================
// WriteFileTool
// ===========================================================================

pub struct WriteFileTool;

#[async_trait]
impl AsyncTool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Write text content to a file (creates or overwrites).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    },
                    "content": {
                        "type": "string",
                        "description": "Text content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("invalid arguments for write_file: {e}"))?;
        let path = args["path"]
            .as_str()
            .ok_or_else(|| "missing required field 'path' in write_file".to_string())?;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| "missing required field 'content' in write_file".to_string())?;
        do_write_file(path, content).await.map_err(|e| e.to_string())?;
        Ok(format!(r#"{{"status":"ok","path":"{}"}}"#, path))
    }
}

// ===========================================================================
// GlobTool
// ===========================================================================

pub struct GlobTool;

#[async_trait]
impl AsyncTool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".into(),
            description: "Find files matching a glob pattern (e.g. '**/*.rs').".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g. 'src/**/*.rs')"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("invalid arguments for glob: {e}"))?;
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| "missing required field 'pattern' in glob".to_string())?;
        let paths = do_glob(pattern).await?;
        Ok(serde_json::to_string(&paths).unwrap_or_else(|_| "[]".to_string()))
    }
}

// ===========================================================================
// GrepTool
// ===========================================================================

pub struct GrepTool;

#[async_trait]
impl AsyncTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Search for a regex pattern in project files.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex or text to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Scope path (optional, defaults to project root)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("invalid arguments for grep: {e}"))?;
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| "missing required field 'pattern' in grep".to_string())?;
        let path = args["path"].as_str().unwrap_or(".");
        do_grep(pattern, path).await
    }
}

// ===========================================================================
// Registration
// ===========================================================================

/// Register all built-in platform tools into the given `ToolRouter`.
pub fn register_platform_tools(router: &mut ToolRouter) {
    router.register(Box::new(ReadFileTool));
    router.register(Box::new(WriteFileTool));
    router.register(Box::new(GlobTool));
    router.register(Box::new(GrepTool));
}

// ===========================================================================
// Helpers — glob matching
// ===========================================================================

/// Split a glob pattern into a base directory and the remaining sub-pattern.
/// E.g. `"src/**/*.rs"` => `("src", "**/*.rs")`, `"*.rs"` => `(".", "*.rs")`.
fn split_glob_base(pattern: &str) -> (std::path::PathBuf, &str) {
    let wildcard_pos = pattern.find(|c: char| c == '*' || c == '?');
    match wildcard_pos {
        None => (std::path::PathBuf::from("."), pattern),
        Some(pos) => match pattern[..pos].rfind('/') {
            None => (std::path::PathBuf::from("."), pattern),
            Some(0) => (std::path::PathBuf::from("/"), &pattern[1..]),
            Some(slash_pos) => {
                let base = std::path::PathBuf::from(&pattern[..slash_pos]);
                let sub = &pattern[slash_pos + 1..];
                (base, sub)
            }
        },
    }
}

/// Check whether `path` matches a glob `pattern` (supports `**`, `*`, `?`).
fn matches_glob(pattern: &str, path: &str) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    matches_glob_parts(&pat_parts, &path_parts, 0, 0)
}

fn matches_glob_parts(pat: &[&str], path: &[&str], pi: usize, ti: usize) -> bool {
    if pi >= pat.len() {
        return ti >= path.len();
    }
    if ti >= path.len() {
        return pat[pi] == "**" && pi + 1 >= pat.len();
    }
    if pat[pi] == "**" {
        // ** matches zero or more path segments
        if matches_glob_parts(pat, path, pi + 1, ti) {
            return true;
        }
        if matches_glob_parts(pat, path, pi, ti + 1) {
            return true;
        }
        return false;
    }
    if matches_single_segment(pat[pi], path[ti]) {
        matches_glob_parts(pat, path, pi + 1, ti + 1)
    } else {
        false
    }
}

fn matches_single_segment(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    matches_segment_chars(&p, &t, 0, 0)
}

fn matches_segment_chars(p: &[char], t: &[char], pi: usize, ti: usize) -> bool {
    if pi >= p.len() {
        return ti >= t.len();
    }
    if ti >= t.len() {
        return p[pi] == '*' && pi + 1 >= p.len();
    }
    match p[pi] {
        '*' => {
            if matches_segment_chars(p, t, pi + 1, ti) {
                return true;
            }
            if matches_segment_chars(p, t, pi, ti + 1) {
                return true;
            }
            false
        }
        '?' => matches_segment_chars(p, t, pi + 1, ti + 1),
        c => {
            if c == t[ti] {
                matches_segment_chars(p, t, pi + 1, ti + 1)
            } else {
                false
            }
        }
    }
}

/// Extensions that `do_grep` is allowed to search.
const GREP_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "json", "yaml", "yml", "txt", "js", "ts", "py", "html", "css",
];

// ===========================================================================
// I/O implementations
// ===========================================================================

/// Read the full contents of a file at `path` as a UTF-8 string.
async fn do_read_file(path: &str) -> Result<String, std::io::Error> {
    tokio::fs::read_to_string(path).await
}

/// Write `content` to a file at `path`, creating/overwriting as needed.
async fn do_write_file(path: &str, content: &str) -> Result<(), std::io::Error> {
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content).await
}

/// Find all files matching the given glob `pattern`.
/// Supports `**`, `*`, and `?` wildcards.
async fn do_glob(pattern: &str) -> Result<Vec<String>, String> {
    let (base_dir, sub_pattern) = split_glob_base(pattern);

    let mut results = Vec::new();
    let mut stack = vec![base_dir.clone()];

    while let Some(dir) = stack.pop() {
        let mut read_dir = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        while let Some(entry) = read_dir.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| e.to_string())?;

            if file_type.is_dir() {
                stack.push(path.clone());
            }

            if file_type.is_file() || file_type.is_dir() {
                if let Ok(rel) = path.strip_prefix(&base_dir) {
                    let rel_str = rel.to_string_lossy();
                    if !rel_str.is_empty() && matches_glob(sub_pattern, &rel_str) {
                        results.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    results.sort();
    Ok(results)
}

/// Search for `pattern` (plain substring) in the subtree rooted at `path`.
/// Only searches files with whitelisted extensions.
/// Returns formatted lines: `file:line: trimmed_content`.
async fn do_grep(pattern: &str, path: &str) -> Result<String, String> {
    let root = if path == "." {
        std::env::current_dir().map_err(|e| format!("failed to get current dir: {e}"))?
    } else {
        std::path::PathBuf::from(path)
    };

    let mut results: Vec<String> = Vec::new();
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let mut read_dir = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        while let Some(entry) = read_dir.next_entry().await.map_err(|e| e.to_string())? {
            let entry_path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| e.to_string())?;

            if file_type.is_dir() {
                stack.push(entry_path);
            } else if file_type.is_file() {
                let ext = entry_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if !GREP_EXTENSIONS.contains(&ext) {
                    continue;
                }

                match tokio::fs::read_to_string(&entry_path).await {
                    Ok(content) => {
                        let rel_path = entry_path.strip_prefix(&root).unwrap_or(&entry_path);
                        let rel_str = rel_path.to_string_lossy();
                        for (line_num, line) in content.lines().enumerate() {
                            if line.contains(pattern) {
                                let trimmed = line.trim();
                                results
                                    .push(format!("{}:{}:{}", rel_str, line_num + 1, trimmed));
                            }
                        }
                    }
                    Err(_) => continue,
                }
            }
        }
    }

    if results.is_empty() {
        Ok(String::new())
    } else {
        Ok(results.join("\n"))
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_file_missing_path() {
        let tool = ReadFileTool;
        let result = tool.execute(r#"{}"#).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required field 'path'"));
    }

    #[tokio::test]
    async fn test_write_file_missing_content() {
        let tool = WriteFileTool;
        let result = tool.execute(r#"{"path":"/tmp/test.txt"}"#).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required field 'content'"));
    }

    #[tokio::test]
    async fn test_glob_missing_pattern() {
        let tool = GlobTool;
        let result = tool.execute(r#"{}"#).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required field 'pattern'"));
    }

    #[tokio::test]
    async fn test_grep_missing_pattern() {
        let tool = GrepTool;
        let result = tool.execute(r#"{"path":"."}"#).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required field 'pattern'"));
    }

    #[test]
    fn test_definitions_have_correct_schema() {
        let tool = ReadFileTool;
        let def = tool.definition();
        assert_eq!(def.name, "read_file");
        assert!(def.parameters["required"].as_array().unwrap().contains(&"path".into()));

        let tool = GlobTool;
        let def = tool.definition();
        assert_eq!(def.name, "glob");
        assert!(def.parameters["required"].as_array().unwrap().contains(&"pattern".into()));

        let tool = GrepTool;
        let def = tool.definition();
        assert_eq!(def.name, "grep");
        assert!(def.parameters["required"].as_array().unwrap().contains(&"pattern".into()));

        let tool = WriteFileTool;
        let def = tool.definition();
        assert_eq!(def.name, "write_file");
        assert!(def.parameters["required"].as_array().unwrap().contains(&"path".into()));
        assert!(def.parameters["required"].as_array().unwrap().contains(&"content".into()));
    }

    #[tokio::test]
    async fn test_register_all() {
        let mut router = ToolRouter::new();
        register_platform_tools(&mut router);
        assert_eq!(router.len(), 4);
        let names = router.tool_names();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
    }

    // -----------------------------------------------------------------------
    // do_* integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_do_read_file_success() {
        let dir = std::env::temp_dir().join("ai_agent_test_do_read");
        let _ = tokio::fs::create_dir_all(&dir).await;
        let file_path = dir.join("hello.txt");
        tokio::fs::write(&file_path, "hello world").await.unwrap();

        let result =
            do_read_file(file_path.to_str().unwrap()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello world");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_do_read_file_not_found() {
        let result = do_read_file("/tmp/__nonexistent_file_42__").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_do_write_file_creates_file() {
        let dir = std::env::temp_dir().join("ai_agent_test_do_write");
        let file_path = dir.join("sub").join("out.txt");
        let path_str = file_path.to_str().unwrap().to_string();

        let result = do_write_file(&path_str, "written content").await;
        assert!(result.is_ok());

        let content = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "written content");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_do_glob_empty_pattern() {
        let result = do_glob("/tmp/__nonexistent_glob_dir__/**/*.xyz").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_do_grep_finds_pattern() {
        let dir = std::env::temp_dir().join("ai_agent_test_do_grep");
        let _ = tokio::fs::create_dir_all(&dir).await;
        let file_path = dir.join("search.rs");
        tokio::fs::write(
            &file_path,
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .await
        .unwrap();

        let result = do_grep("println", dir.to_str().unwrap()).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("search.rs:2:"));
        assert!(output.contains("println"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
