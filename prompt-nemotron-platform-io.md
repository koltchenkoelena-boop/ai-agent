# Task: Implement Platform I/O stubs for AI Agent

## Context

You are working on `src/tool_routing/platform.rs` in the `ai-agent` Rust project.
The file already contains 4 tool structs (`ReadFileTool`, `WriteFileTool`, `GlobTool`, `GrepTool`) with:
- Full `AsyncTool` trait implementations (definition + execute)
- Argument extraction and validation (serde_json::Value)
- `register_platform_tools()` function that registers all 4 into `ToolRouter`

What is MISSING: the actual I/O logic. Each tool's `execute()` calls one of these stub functions:

```rust
async fn do_read_file(path: &str) -> Result<String, std::io::Error> {
    todo!("do_read_file({path}) — implement with tokio::fs::read_to_string")
}

async fn do_write_file(path: &str, _content: &str) -> Result<(), std::io::Error> {
    todo!("do_write_file({path}) — implement with tokio::fs::write")
}

async fn do_glob(pattern: &str) -> Result<Vec<String>, String> {
    todo!("do_glob({pattern}) — implement with `glob` crate or tokio::fs::read_dir walk")
}

async fn do_grep(pattern: &str, path: &str) -> Result<String, String> {
    todo!("do_grep({pattern}, {path}) — implement with ripgrep or manual line scan")
}
```

Your task is to **replace each `todo!()` with real async I/O logic**.

## Requirements

### 1. `do_read_file(path)`
- Use `tokio::fs::read_to_string(path)` to read the file.
- Return the file contents as `String`.
- Propagate `std::io::Error` on failure.

### 2. `do_write_file(path, content)`
- Use `tokio::fs::write(path, content)` to write the file.
- Create parent directories if they don't exist (`tokio::fs::create_dir_all` for the parent).
- Return `Ok(())` on success.

### 3. `do_glob(pattern)`
- Use the `glob` crate (`glob::glob`) to match files.
  - If `glob` is not in `Cargo.toml`, use manual walk: `tokio::fs::read_dir` recursively + `glob_match` from the `glob` crate or a simple `Pattern` match.
  - Actually, the simplest approach: use `glob::glob` (synchronous but fast for listing), convert paths to strings.
- Return `Vec<String>` of matching paths (sorted).
- On error, return `Err(String)` with a description.

### 4. `do_grep(pattern, path)`
- Walk `path` recursively using `tokio::fs::read_dir`.
- For each file with extension `.rs`, `.toml`, `.md`, `.json`, `.yaml`, `.yml`, `.txt`, `.js`, `.ts`, `.py`, `.html`, `.css` — read it and search line by line for `pattern` (treat as plain text substring match for simplicity; regex would require the `regex` crate).
- Format output as `file_path:line_number: trimmed_line_content`.
- If `path == "."`, resolve to current working directory.
- Return formatted string, or empty string if no matches.
- On error, return `Err(String)`.

## Constraints

- **Async**: all functions must be `async fn`. Use tokio's async fs module.
- **Error handling**: `do_read_file` / `do_write_file` return `std::io::Error`. `do_glob` / `do_grep` return `String` errors.
- **No new dependencies added to Cargo.toml** unless essential. The `glob` crate is optional — you may implement glob manually with `tokio::fs::read_dir` walk + simple pattern matching (support `*`, `**`, `?`). If you add `glob`, also add it to `Cargo.toml` under `[dependencies]`.
- **Windows compatibility desirable but not required** — use `/` paths.
- **No external crates for grep** — manual line-by-line search is fine.
- **Tests**: After implementing, add `#[cfg(test)]` tests:
  - `test_do_read_file_success`: create a temp file, read it, verify content
  - `test_do_read_file_not_found`: expect error
  - `test_do_write_file_creates_file`: write, then read back
  - `test_do_glob_empty_pattern`: returns empty vec (no match)
  - `test_do_grep_finds_pattern`: create a temp file with known content, grep for it
  - Use `tempfile` crate for temp directories, OR create files in `std::env::temp_dir()`.

## File to edit

`/home/avk/workspace/ai-agent/src/tool_routing/platform.rs`

The stub functions are at the bottom of the file, after `register_platform_tools()`.
The test module `#[cfg(test)] mod tests { ... }` is already there with 6 tests. Add your new tests inside the same module.

## Verification

After making changes, run:
```bash
cargo test --lib tool_routing::platform
```
All existing and new tests must pass with zero compiler warnings.

## Final note

Do NOT change the tool structs (`ReadFileTool`, `WriteFileTool`, etc.), their `AsyncTool` impls, or `register_platform_tools()`. Only replace the `todo!()` bodies in the 4 `do_*` functions and add tests.
