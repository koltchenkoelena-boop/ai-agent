use std::collections::HashMap;

use crate::types::{Message, Role};

// ---------------------------------------------------------------------------
// Branch
// ---------------------------------------------------------------------------

/// A single named branch holding an ordered list of messages.
#[derive(Debug, Clone)]
pub struct Branch {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Human-readable name (like a Git branch name).
    pub name: String,
    /// Conversation history.
    pub messages: Vec<Message>,
    /// Optional parent branch id — tracks where this branch was forked from.
    pub parent_branch_id: Option<String>,
    /// Index into `parent_branch.messages` at the fork point.
    pub fork_point: Option<usize>,
}

impl Branch {
    pub fn new(id: String, name: String) -> Self {
        Self {
            id,
            name,
            messages: Vec::new(),
            parent_branch_id: None,
            fork_point: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Merge strategy
// ---------------------------------------------------------------------------

/// How to merge one branch into another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Replace the target branch's messages entirely with the source branch.
    Overwrite,
    /// Append only messages that are *new* in the source (those after the
    /// fork point) to the target branch.
    FastForward,
    /// Produce a union of both branches keeping the longest common prefix:
    /// used when both branches have diverged and we want the target to absorb
    /// the source's divergent tail.
    Union,
}

// ---------------------------------------------------------------------------
// ContextManager
// ---------------------------------------------------------------------------

/// Git‑like branching context manager for isolating sub‑task conversations.
///
/// # Semantics
/// - There is always a **current branch** whose messages are fed to the LLM.
/// - `create_branch` forks a new branch from the current branch's *last*
///   message, cloning the history up to that point.
/// - `switch` changes the current branch.
/// - `merge` absorbs messages from a source branch according to the chosen
///   `MergeStrategy`.
/// - Branches can be deleted. Deleting the current branch or the last
///   remaining branch is forbidden.
pub struct ContextManager {
    branches: HashMap<String, Branch>,
    current_id: String,
}

impl ContextManager {
    /// Create a new manager with a single root branch named `"main"`.
    pub fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let mut branches = HashMap::new();
        branches.insert(id.clone(), Branch::new(id.clone(), "main".into()));
        Self {
            branches,
            current_id: id,
        }
    }

    // ---- Branch queries ----------------------------------------------------

    pub fn current_branch(&self) -> &Branch {
        // INVARIANT: current_id always points to an existing branch.
        &self.branches[&self.current_id]
    }

    pub fn current_branch_mut(&mut self) -> &mut Branch {
        self.branches.get_mut(&self.current_id).expect("current_id invariant broken")
    }

    pub fn get(&self, id: &str) -> Option<&Branch> {
        self.branches.get(id)
    }

    /// Human-friendly branch listing.
    pub fn list(&self) -> Vec<&Branch> {
        let mut v: Vec<&Branch> = self.branches.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Number of active branches.
    pub fn len(&self) -> usize {
        self.branches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.branches.is_empty()
    }

    // ---- Current messages (convenience) ------------------------------------

    /// Slice of messages on the current branch.
    pub fn current_messages(&self) -> &[Message] {
        &self.current_branch().messages
    }

    /// Append a single message to the current branch.
    pub fn push(&mut self, msg: Message) {
        self.current_branch_mut().messages.push(msg);
    }

    /// Extend the current branch with several messages.
    pub fn extend(&mut self, msgs: impl IntoIterator<Item = Message>) {
        self.current_branch_mut().messages.extend(msgs);
    }

    // ---- Branches ----------------------------------------------------------

    /// Create a new branch forked from the current branch's tip, then switch
    /// to it (analogous to `git checkout -b`).
    ///
    /// The new branch inherits a *copy* of every message on the current
    /// branch up to and including the fork point.
    pub fn create_branch(&mut self, name: &str) -> &Branch {
        let id = uuid::Uuid::new_v4().to_string();
        let fork_point = self.current_branch().messages.len();

        let mut branch = Branch::new(id.clone(), name.to_string());
        branch.parent_branch_id = Some(self.current_id.clone());
        branch.fork_point = Some(fork_point);
        branch.messages = self.current_branch().messages.clone();

        self.branches.insert(id.clone(), branch);
        self.current_id = id;
        self.branches.get(&self.current_id).expect("just inserted")
    }

    /// Switch to a different branch by id.
    ///
    /// # Errors
    /// Returns the name of the (invalid) id if it does not exist.
    pub fn switch(&mut self, id: String) -> Result<(), String> {
        if self.branches.contains_key(&id) {
            self.current_id = id;
            Ok(())
        } else {
            Err(id)
        }
    }

    /// Switch to a branch by name.
    ///
    /// # Panics
    /// If more than one branch shares the name (treated as a bug).
    pub fn switch_by_name(&mut self, name: &str) -> Result<(), String> {
        let id = self.find_id_by_name(name).ok_or_else(|| format!("branch '{name}' not found"))?;
        self.switch(id)
    }

    /// Rename the current branch.
    pub fn rename(&mut self, new_name: &str) {
        self.current_branch_mut().name = new_name.to_string();
    }

    /// Delete a branch by id (forbidden if it is the only branch or the
    /// current one).
    ///
    /// # Errors
    /// - `"last branch"` — refusing to delete the sole remaining branch.
    /// - `"current branch"` — switch away first before deleting.
    pub fn delete(&mut self, id: &str) -> Result<(), &'static str> {
        if self.branches.len() < 2 {
            return Err("last branch");
        }
        if id == self.current_id {
            return Err("current branch");
        }
        self.branches.remove(id);
        Ok(())
    }

    // ---- Merge -------------------------------------------------------------

    /// Merge messages from `source_id` into the current branch.
    pub fn merge(&mut self, source_id: &str, strategy: MergeStrategy) -> Result<(), String> {
        let source = self
            .branches
            .get(source_id)
            .ok_or_else(|| format!("source branch '{source_id}' not found"))?
            .clone();

        let target = self.current_branch_mut();

        match strategy {
            MergeStrategy::Overwrite => {
                target.messages = source.messages;
            }
            MergeStrategy::FastForward => {
                let fork = source.fork_point.unwrap_or(0);
                // Only append messages that exist in source beyond the fork
                // and are not already in target.
                for msg in source.messages.iter().skip(fork) {
                    if !target.messages.contains(msg) {
                        target.messages.push(msg.clone());
                    }
                }
            }
            MergeStrategy::Union => {
                // Longest-common-prefix union: keep shared prefix, then
                // append divergent tails from both sides, deduplicating.
                let prefix_len = target
                    .messages
                    .iter()
                    .zip(source.messages.iter())
                    .take_while(|(a, b)| a == b)
                    .count();

                // Target's own tail
                let tail_target = target.messages[prefix_len..].to_vec();
                // Source's tail (beyond the shared prefix)
                let tail_source = source.messages[prefix_len..].to_vec();

                target.messages.truncate(prefix_len);

                for msg in tail_target {
                    if !target.messages.contains(&msg) {
                        target.messages.push(msg);
                    }
                }
                for msg in tail_source {
                    if !target.messages.contains(&msg) {
                        target.messages.push(msg);
                    }
                }
            }
        }
        Ok(())
    }

    // ---- Snapshot / restore ------------------------------------------------

    /// Serialisable snapshot of every branch.
    pub fn snapshot(&self) -> HashMap<String, (String, Vec<Message>)> {
        self.branches
            .iter()
            .map(|(id, b)| (id.clone(), (b.name.clone(), b.messages.clone())))
            .collect()
    }

    // ---- Internal helpers --------------------------------------------------

    fn find_id_by_name(&self, name: &str) -> Option<String> {
        self.branches
            .iter()
            .find(|(_, b)| b.name == name)
            .map(|(id, _)| id.clone())
    }
}

// ===========================================================================
// Auto-compaction — эвристическое сжатие контекста
// ===========================================================================

/// Конфигурация автоматической компакции контекста.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Максимум сообщений на ветке до триггера компакции.
    pub max_messages: usize,
    /// Сколько последних сообщений сохранять нетронутыми.
    pub reserve_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            max_messages: 15,
            reserve_recent: 4,
        }
    }
}

impl ContextManager {
    /// Проверить, превышен ли лимит сообщений на текущей ветке.
    pub fn needs_compaction(&self, config: &CompactionConfig) -> bool {
        self.current_branch().messages.len() > config.max_messages
    }

    /// Вернуть `(start, end)` — диапазон сообщений, подлежащих суммаризации.
    ///
    /// - Пропускает системное сообщение (первый `Role::System`, если есть).
    /// - Оставляет последние `reserve_recent` сообщений нетронутыми.
    /// - Возвращает `None`, если сжимать нечего (диапазон пуст или лимит не превышен).
    pub fn compaction_range(&self, config: &CompactionConfig) -> Option<(usize, usize)> {
        if !self.needs_compaction(config) {
            return None;
        }

        let msgs = &self.current_branch().messages;
        let total = msgs.len();

        // Пропускаем системный промпт
        let start = msgs.iter().position(|m| m.role == Role::System)
            .map(|i| i + 1)
            .unwrap_or(0);

        // Оставляем последние reserve_recent
        let end = total.saturating_sub(config.reserve_recent);

        if start >= end || start >= total {
            return None;
        }

        Some((start, end))
    }

    /// Заменить сообщения `[start..end)` на одно summarised-сообщение
    /// с ролью `System`.
    ///
    /// После вызова размер ветки уменьшается на `(end - start - 1)`.
    pub fn compact(&mut self, summary: String, start: usize, end: usize) {
        let branch = self.current_branch_mut();
        let summary_msg = Message::new(Role::System, summary);
        branch.messages.splice(start..end, std::iter::once(summary_msg));
    }
}

// ---------------------------------------------------------------------------
// Default
// ---------------------------------------------------------------------------

impl Default for ContextManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn system_msg(s: &str) -> Message {
        Message::new(Role::System, s)
    }

    fn user_msg(s: &str) -> Message {
        Message::new(Role::User, s)
    }

    #[test]
    fn test_new_has_main_branch() {
        let cm = ContextManager::new();
        assert_eq!(cm.len(), 1);
        assert_eq!(cm.current_branch().name, "main");
        assert!(cm.current_messages().is_empty());
    }

    #[test]
    fn test_push_message() {
        let mut cm = ContextManager::new();
        cm.push(user_msg("hello"));
        assert_eq!(cm.current_messages().len(), 1);
    }

    #[test]
    fn test_create_and_switch_branch() {
        let mut cm = ContextManager::new();

        cm.push(system_msg("sys1"));
        cm.push(user_msg("user1"));

        // fork from tip (2 messages)
        cm.create_branch("experiment");
        let exp_id = cm.find_id_by_name("experiment").unwrap();
        assert_eq!(cm.get(&exp_id).unwrap().messages.len(), 2);

        // push on the new branch (still current)
        cm.push(user_msg("exp-specific"));
        assert_eq!(cm.current_messages().len(), 3);

        // switch back to main
        let main_id = cm.find_id_by_name("main").unwrap();
        cm.switch(main_id.clone()).unwrap();
        assert_eq!(cm.current_messages().len(), 2); // original messages only
        assert_eq!(cm.current_branch().name, "main");
    }

    #[test]
    fn test_merge_fast_forward() {
        let mut cm = ContextManager::new();

        cm.push(system_msg("sys1"));
        cm.push(user_msg("q1"));

        cm.create_branch("dev");
        cm.push(user_msg("dev-only"));

        // switch to main and merge dev
        let main_id = cm.find_id_by_name("main").unwrap();
        cm.switch(main_id).unwrap();
        let dev_id = cm.find_id_by_name("dev").unwrap();
        cm.merge(&dev_id, MergeStrategy::FastForward).unwrap();

        assert_eq!(cm.current_messages().len(), 3);
        assert_eq!(cm.current_messages()[2].content.as_deref(), Some("dev-only"));
    }

    #[test]
    fn test_merge_overwrite() {
        let mut cm = ContextManager::new();

        cm.push(system_msg("sys1"));
        cm.push(user_msg("q1"));

        cm.create_branch("dev");
        cm.push(user_msg("dev-only"));

        // switch to main and overwrite
        let main_id = cm.find_id_by_name("main").unwrap();
        cm.switch(main_id).unwrap();
        let dev_id = cm.find_id_by_name("dev").unwrap();
        cm.merge(&dev_id, MergeStrategy::Overwrite).unwrap();

        assert_eq!(cm.current_messages().len(), 3);
    }

    #[test]
    fn test_merge_union() {
        let mut cm = ContextManager::new();

        // Shared prefix: sys, q1
        cm.push(system_msg("sys"));
        cm.push(user_msg("q1"));

        // Fork, each side adds its own messages
        cm.create_branch("side-a");
        cm.push(user_msg("a-only"));

        let main_id = cm.find_id_by_name("main").unwrap();
        cm.switch(main_id).unwrap();
        cm.push(user_msg("main-only"));

        let side_a = cm.find_id_by_name("side-a").unwrap();
        cm.merge(&side_a, MergeStrategy::Union).unwrap();

        // Both "a-only" and "main-only" should be present after the prefix
        let contents: Vec<Option<String>> = cm
            .current_messages()
            .iter()
            .map(|m| m.content.clone())
            .collect();
        assert!(contents.contains(&Some("a-only".into())));
        assert!(contents.contains(&Some("main-only".into())));
    }

    #[test]
    fn test_delete_branch() {
        let mut cm = ContextManager::new();
        cm.create_branch("tmp");
        cm.create_branch("tmp2");

        let tmp_id = cm.find_id_by_name("tmp").unwrap();
        assert!(cm.delete(&tmp_id).is_ok());
        assert_eq!(cm.len(), 2); // main + tmp2 remain
    }

    #[test]
    fn test_delete_refuses_current_and_last() {
        let mut cm = ContextManager::new();
        // Only one branch → refuse
        let main_id = cm.find_id_by_name("main").unwrap();
        assert_eq!(cm.delete(&main_id), Err("last branch"));

        cm.create_branch("tmp");
        // current is still "tmp" (created by create_branch)
        let tmp_id = cm.find_id_by_name("tmp").unwrap();
        assert_eq!(cm.delete(&tmp_id), Err("current branch"));
    }

    #[test]
    fn test_switch_by_name_not_found() {
        let mut cm = ContextManager::new();
        assert_eq!(
            cm.switch_by_name("nonexistent"),
            Err("branch 'nonexistent' not found".into())
        );
    }

    #[test]
    fn test_rename() {
        let mut cm = ContextManager::new();
        cm.rename("production");
        assert_eq!(cm.current_branch().name, "production");
    }

    #[test]
    fn test_snapshot_round_trip() {
        let mut cm = ContextManager::new();
        cm.push(user_msg("hello"));
        cm.create_branch("feature");

        let snap = cm.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.values().any(|(name, msgs)| name == "feature" && msgs.len() == 1));
    }

    // =======================================================================
    // Auto-compaction tests
    // =======================================================================

    #[test]
    fn test_needs_compaction_below_threshold() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 10,
            reserve_recent: 3,
        };
        for i in 0..8 {
            cm.push(user_msg(&format!("msg {i}")));
        }
        assert!(!cm.needs_compaction(&config));
    }

    #[test]
    fn test_needs_compaction_above_threshold() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 5,
            reserve_recent: 2,
        };
        for i in 0..8 {
            cm.push(user_msg(&format!("msg {i}")));
        }
        assert!(cm.needs_compaction(&config));
    }

    #[test]
    fn test_compaction_range_returns_none_if_no_compaction_needed() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig::default();
        for i in 0..10 {
            cm.push(user_msg(&format!("msg {i}")));
        }
        assert!(cm.compaction_range(&config).is_none());
    }

    #[test]
    fn test_compaction_range_skips_system_and_reserve_recent() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 5,
            reserve_recent: 2,
        };
        cm.push(system_msg("You are a bot."));
        for i in 0..10 {
            cm.push(user_msg(&format!("msg {i}")));
        }

        let range = cm.compaction_range(&config).unwrap();
        assert_eq!(range, (1, 9));
    }

    #[test]
    fn test_compaction_range_respects_system_position() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 4,
            reserve_recent: 1,
        };
        for i in 0..6 {
            cm.push(user_msg(&format!("msg {i}")));
        }
        let range = cm.compaction_range(&config).unwrap();
        assert_eq!(range, (0, 5));
    }

    #[test]
    fn test_compact_reduces_message_count() {
        let mut cm = ContextManager::new();
        cm.push(system_msg("System init."));
        for i in 0..10 {
            cm.push(user_msg(&format!("msg {i}")));
        }
        assert_eq!(cm.current_messages().len(), 11);

        cm.compact("Summary of previous conversation.".into(), 1, 9);
        // splice(1..9, [summary]): удаляем 8 msgs, вставляем 1 = 11 - 7 = 4
        assert_eq!(cm.current_messages().len(), 4);
        assert_eq!(
            cm.current_messages()[1].content.as_deref(),
            Some("Summary of previous conversation.")
        );
        assert_eq!(cm.current_messages()[1].role, Role::System);
    }

    #[test]
    fn test_full_compaction_cycle() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 6,
            reserve_recent: 2,
        };

        cm.push(system_msg("System prompt."));
        for i in 0..10 {
            cm.push(user_msg(&format!("msg {i}")));
        }

        assert!(cm.needs_compaction(&config));
        let (start, end) = cm.compaction_range(&config).unwrap();

        let summary = format!("Compacted {} messages.", end - start);
        cm.compact(summary, start, end);

        assert_eq!(cm.current_messages().len(), 4);
        assert_eq!(
            cm.current_messages()[0].content.as_deref(),
            Some("System prompt.")
        );
        assert!(cm.current_messages()[1]
            .content
            .as_deref()
            .unwrap()
            .contains("Compacted"));
    }

    #[test]
    fn test_compaction_no_system_message() {
        let mut cm = ContextManager::new();
        let config = CompactionConfig {
            max_messages: 3,
            reserve_recent: 1,
        };

        for i in 0..6 {
            cm.push(user_msg(&format!("msg {i}")));
        }

        let (start, end) = cm.compaction_range(&config).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 5);

        cm.compact("Early messages.".into(), start, end);
        assert_eq!(cm.current_messages().len(), 2);
    }
}
