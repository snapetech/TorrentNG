use anyhow::Result;
use rusqlite::{params, types::Value, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::{db::Db, AutomationStateCapacityError, MAX_AUTOMATION_STATE_JSON_BYTES};

const KEY: &str = "workflow_rules";
const RUNS_KEY: &str = "workflow_runs";
const RSS_KEY: &str = "rss_rules";
const QBIT_RSS_ITEMS_KEY: &str = "qbit_rss_items";
const MAX_QBIT_RSS_ITEMS_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_QBIT_RSS_ITEMS_READ_BYTES: usize = 128 * 1024 * 1024;
const MAX_RUNS: usize = 200;
const MAX_WORKFLOW_RUNS_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKFLOW_RUN_SAMPLE_ITEMS: usize = 32;
const MAX_WORKFLOW_RUN_ID_BYTES: usize = 128;
const MAX_WORKFLOW_RUN_NAME_BYTES: usize = 256;
const MAX_WORKFLOW_RUN_ACTION_BYTES: usize = 64;
const MAX_WORKFLOW_RUN_ITEM_BYTES: usize = 256;
const MAX_WORKFLOW_RUN_ERROR_BYTES: usize = 512;
const MAX_RSS_RULES: usize = 1_024;

#[derive(Debug, thiserror::Error)]
#[error("qBittorrent RSS item state exceeds the configured byte limit")]
pub struct QbitRssItemCapacityError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub event: String,
    pub action: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub target_category: Option<String>,
    pub tracker: Option<String>,
    pub command: Option<String>,
    pub url: Option<String>,
    pub target_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRun {
    pub id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub action: String,
    pub dry_run: bool,
    pub matched: Vec<String>,
    #[serde(default)]
    pub matched_total: usize,
    pub applied: Vec<String>,
    #[serde(default)]
    pub applied_total: usize,
    pub errors: Vec<String>,
    #[serde(default)]
    pub errors_total: usize,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RssRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub feed_url: String,
    pub include: String,
    pub exclude: Option<String>,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub tags: Vec<String>,
    pub start: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RssRuleMatch {
    pub rule_id: String,
    pub rule_name: String,
    pub matched: bool,
    pub reason: String,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub tags: Vec<String>,
    pub start: bool,
}

impl Db {
    pub fn list_workflow_rules(&self) -> Result<Vec<WorkflowRule>> {
        let conn = self.read()?;
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut rules: Vec<WorkflowRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        for rule in &mut rules {
            normalize_legacy_category_target(rule);
        }
        rules.sort_by_key(|a| a.name.to_lowercase());
        Ok(rules)
    }

    pub fn upsert_workflow_rule(&self, mut rule: WorkflowRule) -> Result<Vec<WorkflowRule>> {
        normalize_legacy_category_target(&mut rule);
        if rule.id.trim().is_empty() {
            rule.id = uuid::Uuid::new_v4().to_string();
        }
        let id = rule.id.clone();
        self.update_workflow_rules(|rules| {
            rules.retain(|existing| existing.id != id);
            rules.push(rule);
        })
    }

    pub fn get_workflow_rule(&self, id: &str) -> Result<Option<WorkflowRule>> {
        Ok(self
            .list_workflow_rules()?
            .into_iter()
            .find(|rule| rule.id == id))
    }

    pub fn workflow_hashes(&self, rule: &WorkflowRule, limit: usize) -> Result<Vec<String>> {
        let conn = self.read()?;
        let mut clauses = Vec::new();
        let mut args = Vec::new();

        match rule.event.as_str() {
            "completed" => clauses.push("complete != 0".to_owned()),
            "added" => {}
            "category_changed" => {}
            _ => {}
        }
        if let Some(category) = &rule.category {
            clauses.push(format!("category = ?{}", args.len() + 1));
            args.push(Value::Text(category.clone()));
        }
        if let Some(tracker) = &rule.tracker {
            clauses.push(format!(
                "instr(lower(tracker_url), lower(?{})) > 0",
                args.len() + 1
            ));
            args.push(Value::Text(tracker.clone()));
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let limit_parameter = args.len() + 1;
        args.push(Value::Integer(i64::try_from(limit)?));
        let mut stmt = conn.prepare(&format!(
            "SELECT hash FROM torrents{where_sql} ORDER BY name COLLATE NOCASE LIMIT ?{limit_parameter}"
        ))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn delete_workflow_rule(&self, id: &str) -> Result<Vec<WorkflowRule>> {
        self.update_workflow_rules(|rules| {
            rules.retain(|existing| existing.id != id);
        })
    }

    pub fn list_workflow_runs(&self) -> Result<Vec<WorkflowRun>> {
        let conn = self.read()?;
        let mut runs = read_workflow_runs(&conn, false)?;
        for run in &mut runs {
            normalize_workflow_run_totals(run);
        }
        runs.sort_by_key(|run| {
            (
                std::cmp::Reverse(run.started_at),
                std::cmp::Reverse(run.id.clone()),
            )
        });
        runs.truncate(MAX_RUNS);
        Ok(runs)
    }

    pub fn record_workflow_run(&self, mut run: WorkflowRun) -> Result<Vec<WorkflowRun>> {
        bound_workflow_run(&mut run);

        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut runs = read_workflow_runs(&tx, true)?;
        for existing in &mut runs {
            bound_workflow_run(existing);
        }
        runs.push(run);
        runs.sort_by_key(|run| {
            (
                std::cmp::Reverse(run.started_at),
                std::cmp::Reverse(run.id.clone()),
            )
        });
        runs.truncate(MAX_RUNS);

        let raw = serialize_workflow_runs_bounded(&mut runs)?;
        tx.execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![RUNS_KEY, raw],
        )?;
        tx.commit()?;
        Ok(runs)
    }

    pub fn list_rss_rules(&self) -> Result<Vec<RssRule>> {
        let conn = self.read()?;
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![RSS_KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut rules: Vec<RssRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        rules.sort_by_key(|a| a.name.to_lowercase());
        Ok(rules)
    }

    pub fn upsert_rss_rule(&self, mut rule: RssRule) -> Result<RssRuleUpsertResult> {
        if rule.id.trim().is_empty() {
            rule.id = uuid::Uuid::new_v4().to_string();
        }
        let id = rule.id.clone();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![RSS_KEY],
                |row| row.get(0),
            )
            .optional()?;
        let mut rules: Vec<RssRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        if !rules.iter().any(|existing| existing.id == id) && rules.len() >= MAX_RSS_RULES {
            return Ok(RssRuleUpsertResult::Capacity);
        }
        rules.retain(|existing| existing.id != id);
        rules.push(rule);
        rules.sort_by_key(|existing| existing.name.to_lowercase());
        write_json_vec(&tx, RSS_KEY, &rules)?;
        tx.commit()?;
        Ok(RssRuleUpsertResult::Upserted(rules))
    }

    pub fn upsert_qbit_rss_rule(&self, mut rule: RssRule) -> Result<RssRuleUpsertResult> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![RSS_KEY],
                |row| row.get(0),
            )
            .optional()?;
        let mut rules: Vec<RssRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };

        if let Some(existing_id) = rules
            .iter()
            .find(|existing| existing.name == rule.name)
            .map(|existing| existing.id.clone())
        {
            rules.retain(|existing| existing.name != rule.name);
            rule.id = existing_id;
        } else {
            if rules.len() >= MAX_RSS_RULES {
                return Ok(RssRuleUpsertResult::Capacity);
            }
            if rule.id.trim().is_empty() {
                rule.id = uuid::Uuid::new_v4().to_string();
            }
        }
        rules.push(rule);
        rules.sort_by_key(|existing| existing.name.to_lowercase());
        write_json_vec(&tx, RSS_KEY, &rules)?;
        tx.commit()?;
        Ok(RssRuleUpsertResult::Upserted(rules))
    }

    pub fn delete_rss_rule(&self, id: &str) -> Result<Vec<RssRule>> {
        self.update_rss_rules(|rules| {
            rules.retain(|existing| existing.id != id);
        })
    }

    pub fn rename_rss_rule(&self, old_name: &str, new_name: &str) -> Result<RssRuleRenameResult> {
        let mut result = RssRuleRenameResult::Missing;
        self.update_rss_rules(|rules| {
            let Some(rule_index) = rules.iter().position(|rule| rule.name == old_name) else {
                return;
            };
            if old_name != new_name && rules.iter().any(|rule| rule.name == new_name) {
                result = RssRuleRenameResult::Conflict;
                return;
            }
            rules[rule_index].name = new_name.to_owned();
            result = RssRuleRenameResult::Renamed;
        })?;
        Ok(result)
    }

    pub fn delete_rss_rule_by_name(&self, name: &str) -> Result<bool> {
        let mut removed = false;
        self.update_rss_rules(|rules| {
            let before = rules.len();
            rules.retain(|rule| rule.name != name);
            removed = rules.len() != before;
        })?;
        Ok(removed)
    }

    pub fn list_qbit_rss_items(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let conn = self.read()?;
        read_qbit_rss_items(&conn)
    }

    pub fn update_qbit_rss_items<T, F>(&self, update: F) -> Result<T>
    where
        F: FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> T,
    {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut items = read_qbit_rss_items(&tx)?;
        let result = update(&mut items);
        let serialized = serde_json::to_string(&items)?;
        if serialized.len() > MAX_QBIT_RSS_ITEMS_JSON_BYTES {
            return Err(QbitRssItemCapacityError.into());
        }
        tx.execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![QBIT_RSS_ITEMS_KEY, serialized],
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn match_rss_item(&self, title: &str, link: Option<&str>) -> Result<Vec<RssRuleMatch>> {
        let haystack = format!(
            "{} {}",
            title.to_lowercase(),
            link.unwrap_or("").to_lowercase()
        );
        let matches = self
            .list_rss_rules()?
            .into_iter()
            .map(|rule| {
                let include_ok = pattern_list_matches(&rule.include, &haystack);
                let exclude_hit = rule
                    .exclude
                    .as_deref()
                    .map(|exclude| pattern_list_matches(exclude, &haystack))
                    .unwrap_or(false);
                let (matched, reason) = if !rule.enabled {
                    (false, "rule disabled".to_owned())
                } else if !include_ok {
                    (false, "include pattern did not match".to_owned())
                } else if exclude_hit {
                    (false, "exclude pattern matched".to_owned())
                } else {
                    (true, "matched".to_owned())
                };
                RssRuleMatch {
                    rule_id: rule.id,
                    rule_name: rule.name,
                    matched,
                    reason,
                    category: rule.category,
                    save_path: rule.save_path,
                    tags: rule.tags,
                    start: rule.start,
                }
            })
            .collect();
        Ok(matches)
    }

    fn update_workflow_rules<F>(&self, update: F) -> Result<Vec<WorkflowRule>>
    where
        F: FnOnce(&mut Vec<WorkflowRule>),
    {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut rules: Vec<WorkflowRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        for rule in &mut rules {
            normalize_legacy_category_target(rule);
        }
        update(&mut rules);
        rules.sort_by_key(|rule| rule.name.to_lowercase());
        write_json_vec(&tx, KEY, &rules)?;
        tx.commit()?;
        Ok(rules)
    }

    fn update_rss_rules<F>(&self, update: F) -> Result<Vec<RssRule>>
    where
        F: FnOnce(&mut Vec<RssRule>),
    {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key=?1", params![RSS_KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut rules: Vec<RssRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        update(&mut rules);
        rules.sort_by_key(|rule| rule.name.to_lowercase());
        write_json_vec(&tx, RSS_KEY, &rules)?;
        tx.commit()?;
        Ok(rules)
    }
}

fn normalize_legacy_category_target(rule: &mut WorkflowRule) {
    // Older rule payloads stored the set_category target in `category`, which
    // is now reserved for matching/filtering. Move that value to the target
    // field and avoid filtering out torrents that need the category change.
    if rule.action == "set_category" && rule.target_category.is_none() {
        rule.target_category = rule.category.take();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssRuleRenameResult {
    Missing,
    Conflict,
    Renamed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RssRuleUpsertResult {
    Upserted(Vec<RssRule>),
    Capacity,
}

fn write_json_vec<T: Serialize>(tx: &Transaction<'_>, key: &str, values: &[T]) -> Result<()> {
    let raw = serde_json::to_string(values)?;
    if raw.len() > MAX_AUTOMATION_STATE_JSON_BYTES {
        return Err(AutomationStateCapacityError.into());
    }
    tx.execute(
        "INSERT INTO kv(key, value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, raw],
    )?;
    Ok(())
}

fn pattern_list_matches(patterns: &str, haystack: &str) -> bool {
    patterns
        .split([',', '\n'])
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| haystack.contains(&pattern.to_lowercase()))
}

fn read_workflow_runs(conn: &Connection, reset_oversized: bool) -> Result<Vec<WorkflowRun>> {
    let row: Option<(Option<String>, i64)> = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END,
                    length(CAST(value AS BLOB))
             FROM kv WHERE key=?1",
            params![RUNS_KEY, MAX_WORKFLOW_RUNS_JSON_BYTES as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((raw, byte_len)) = row else {
        return Ok(Vec::new());
    };
    if byte_len > MAX_WORKFLOW_RUNS_JSON_BYTES as i64 {
        if reset_oversized {
            // Run history is a bounded diagnostic cache, not authoritative
            // torrent state. Replace an oversized legacy value on the next
            // write instead of repeatedly allocating/parsing it.
            tracing::warn!(
                component = "cache",
                operation = "workflow_runs",
                result = "reset_oversized_history",
                byte_len,
                "discarding oversized legacy workflow run history"
            );
            return Ok(Vec::new());
        }
        anyhow::bail!("workflow run history exceeds the {MAX_WORKFLOW_RUNS_JSON_BYTES} byte limit");
    }
    match raw {
        Some(raw) => Ok(serde_json::from_str(&raw)?),
        None => Ok(Vec::new()),
    }
}

fn read_qbit_rss_items(conn: &Connection) -> Result<serde_json::Map<String, serde_json::Value>> {
    let row: Option<(Option<String>, i64)> = conn
        .query_row(
            "SELECT CASE WHEN length(CAST(value AS BLOB)) <= ?2 THEN value END,
                    length(CAST(value AS BLOB))
             FROM kv WHERE key=?1",
            params![QBIT_RSS_ITEMS_KEY, MAX_QBIT_RSS_ITEMS_READ_BYTES as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((raw, byte_len)) = row else {
        return Ok(serde_json::Map::new());
    };
    if byte_len > MAX_QBIT_RSS_ITEMS_READ_BYTES as i64 {
        anyhow::bail!("qBittorrent RSS item state exceeds the read limit");
    }
    match raw {
        Some(raw) => Ok(serde_json::from_str(&raw)?),
        None => Ok(serde_json::Map::new()),
    }
}

fn normalize_workflow_run_totals(run: &mut WorkflowRun) {
    // Old rows predate the explicit totals. Their full result arrays are the
    // best available counts; new rows retain the original totals even when
    // their sample arrays have been shortened.
    run.matched_total = run.matched_total.max(run.matched.len());
    run.applied_total = run.applied_total.max(run.applied.len());
    run.errors_total = run.errors_total.max(run.errors.len());
}

fn bound_workflow_run(run: &mut WorkflowRun) {
    normalize_workflow_run_totals(run);

    truncate_utf8(&mut run.id, MAX_WORKFLOW_RUN_ID_BYTES);
    truncate_utf8(&mut run.rule_id, MAX_WORKFLOW_RUN_ID_BYTES);
    truncate_utf8(&mut run.rule_name, MAX_WORKFLOW_RUN_NAME_BYTES);
    truncate_utf8(&mut run.action, MAX_WORKFLOW_RUN_ACTION_BYTES);
    run.matched
        .retain(|value| value.len() <= MAX_WORKFLOW_RUN_ITEM_BYTES);
    run.matched.truncate(MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
    run.applied
        .retain(|value| value.len() <= MAX_WORKFLOW_RUN_ITEM_BYTES);
    run.applied.truncate(MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
    run.errors.truncate(MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
    for error in &mut run.errors {
        truncate_utf8(error, MAX_WORKFLOW_RUN_ERROR_BYTES);
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn serialize_workflow_runs_bounded(runs: &mut Vec<WorkflowRun>) -> Result<String> {
    let mut encoded = Vec::with_capacity(runs.len());
    let mut byte_len = 2usize.saturating_add(runs.len().saturating_sub(1));
    for run in runs.iter() {
        let value = serde_json::to_string(run)?;
        byte_len = byte_len.saturating_add(value.len());
        encoded.push(value);
    }

    // `runs` is newest-first, so remove the oldest serialized entries until
    // both the row count and the JSON byte budget are satisfied.
    while byte_len > MAX_WORKFLOW_RUNS_JSON_BYTES {
        if encoded.len() <= 1 {
            anyhow::bail!("one workflow run exceeds the history byte limit");
        }
        let oldest = encoded.pop().expect("length checked above");
        byte_len = byte_len.saturating_sub(oldest.len() + 1);
        runs.pop();
    }

    Ok(format!("[{}]", encoded.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread};

    fn workflow_run(id: impl Into<String>, started_at: i64) -> WorkflowRun {
        WorkflowRun {
            id: id.into(),
            rule_id: "rule".to_owned(),
            rule_name: "Rule".to_owned(),
            action: "webhook".to_owned(),
            dry_run: false,
            matched: Vec::new(),
            matched_total: 0,
            applied: Vec::new(),
            applied_total: 0,
            errors: Vec::new(),
            errors_total: 0,
            started_at,
        }
    }

    #[test]
    fn concurrent_workflow_updates_do_not_lose_rules() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let start = Arc::new(std::sync::Barrier::new(16));

        thread::scope(|scope| {
            for index in 0..16 {
                let db = db.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    db.upsert_workflow_rule(WorkflowRule {
                        id: format!("rule-{index}"),
                        name: format!("Rule {index}"),
                        enabled: true,
                        event: "added".to_owned(),
                        action: "tag".to_owned(),
                        category: None,
                        target_category: None,
                        tracker: None,
                        command: None,
                        url: None,
                        target_path: None,
                    })
                    .unwrap();
                });
            }
        });

        assert_eq!(db.list_workflow_rules().unwrap().len(), 16);
    }

    #[test]
    fn workflow_hashes_applies_sql_limit() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        {
            let mut conn = db.conn().expect("healthy test writer");
            let tx = conn.transaction().unwrap();
            for index in 0..3 {
                tx.execute(
                    "INSERT INTO torrents(hash, name) VALUES(?1, ?2)",
                    params![format!("hash-{index}"), format!("Torrent {index}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }

        let rule = WorkflowRule {
            id: "all".to_owned(),
            name: "All torrents".to_owned(),
            enabled: true,
            event: "added".to_owned(),
            action: "set_category".to_owned(),
            category: None,
            target_category: None,
            tracker: None,
            command: None,
            url: None,
            target_path: None,
        };
        let hashes = db.workflow_hashes(&rule, 2).unwrap();
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn automation_json_vectors_reject_aggregate_growth_without_replacing_state() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        db.set_kv(KEY, "[]").unwrap();
        let oversized = vec!["x".repeat(MAX_AUTOMATION_STATE_JSON_BYTES + 1)];

        let error = {
            let mut conn = db.conn().expect("healthy test writer");
            let tx = conn.transaction().unwrap();
            let error = write_json_vec(&tx, KEY, &oversized).unwrap_err();
            let raw: String = tx
                .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(raw, "[]");
            tx.commit().unwrap();
            error
        };

        assert!(error.is::<AutomationStateCapacityError>());
    }

    #[test]
    fn workflow_run_history_keeps_bounded_samples_and_full_totals() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let mut run = workflow_run("run", 1);
        run.matched = (0..100).map(|index| format!("hash-{index}")).collect();
        run.applied = run.matched.clone();
        run.errors = (0..100).map(|_| "é".repeat(600)).collect();

        let recorded = db.record_workflow_run(run).unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].matched_total, 100);
        assert_eq!(recorded[0].matched.len(), MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
        assert_eq!(recorded[0].applied_total, 100);
        assert_eq!(recorded[0].applied.len(), MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
        assert_eq!(recorded[0].errors_total, 100);
        assert_eq!(recorded[0].errors.len(), MAX_WORKFLOW_RUN_SAMPLE_ITEMS);
        assert!(recorded[0]
            .errors
            .iter()
            .all(|error| error.len() <= MAX_WORKFLOW_RUN_ERROR_BYTES));

        let listed = db.list_workflow_runs().unwrap();
        assert_eq!(listed[0].matched_total, 100);
        assert_eq!(listed[0].applied_total, 100);
        assert_eq!(listed[0].errors_total, 100);
    }

    #[test]
    fn workflow_run_history_drops_oldest_entries_at_json_byte_limit() {
        let mut runs = (0..MAX_RUNS)
            .map(|index| {
                let mut run = workflow_run(index.to_string(), index as i64);
                run.errors =
                    vec!["\0".repeat(MAX_WORKFLOW_RUN_ERROR_BYTES); MAX_WORKFLOW_RUN_SAMPLE_ITEMS];
                bound_workflow_run(&mut run);
                run
            })
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));

        let raw = serialize_workflow_runs_bounded(&mut runs).unwrap();

        assert!(raw.len() <= MAX_WORKFLOW_RUNS_JSON_BYTES);
        assert!(runs.len() < MAX_RUNS);
        assert_eq!(runs[0].id, (MAX_RUNS - 1).to_string());
        let decoded: Vec<WorkflowRun> = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.len(), runs.len());
    }

    #[test]
    fn oversized_legacy_workflow_history_is_not_parsed_and_next_write_recovers() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let oversized = " ".repeat(MAX_WORKFLOW_RUNS_JSON_BYTES + 1);
        db.set_kv(RUNS_KEY, &oversized).unwrap();

        assert!(db.list_workflow_runs().is_err());
        let recorded = db
            .record_workflow_run(workflow_run("fresh", 1))
            .expect("new records recover bounded diagnostic history");

        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].id, "fresh");
        assert_eq!(db.list_workflow_runs().unwrap().len(), 1);
    }

    #[test]
    fn legacy_workflow_history_derives_totals_from_full_arrays() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        db.set_kv(
            RUNS_KEY,
            r#"[{"id":"old","rule_id":"r","rule_name":"R","action":"webhook","dry_run":true,"matched":["a","b"],"applied":["a"],"errors":[],"started_at":1}]"#,
        )
        .unwrap();

        let runs = db.list_workflow_runs().unwrap();
        assert_eq!(runs[0].matched_total, 2);
        assert_eq!(runs[0].applied_total, 1);
        assert_eq!(runs[0].errors_total, 0);
    }

    #[test]
    fn legacy_set_category_workflow_moves_category_to_target() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        {
            let mut conn = db.conn().expect("healthy test writer");
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO torrents(hash, name, category) VALUES(?1, ?2, ?3)",
                params!["existing", "Existing torrent", "Existing"],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        db.set_kv(
            KEY,
            &serde_json::json!([{
                "id": "legacy",
                "name": "Legacy category rule",
                "enabled": true,
                "event": "added",
                "action": "set_category",
                "category": "Target",
                "tracker": null,
                "command": null,
                "url": null,
                "target_path": null
            }])
            .to_string(),
        )
        .unwrap();

        let rule = db.get_workflow_rule("legacy").unwrap().unwrap();
        assert_eq!(rule.category, None);
        assert_eq!(rule.target_category.as_deref(), Some("Target"));
        assert_eq!(db.workflow_hashes(&rule, 10).unwrap(), vec!["existing"]);
    }

    #[test]
    fn qbit_rss_items_persist_and_concurrent_updates_do_not_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let db = Db::open(&path).unwrap();
        let start = Arc::new(std::sync::Barrier::new(16));

        thread::scope(|scope| {
            for index in 0..16 {
                let db = db.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    db.update_qbit_rss_items(|items| {
                        items.insert(
                            format!("feed-{index}"),
                            serde_json::json!({"type": "feed", "index": index}),
                        );
                    })
                    .unwrap();
                });
            }
        });

        assert_eq!(db.list_qbit_rss_items().unwrap().len(), 16);
        drop(db);

        let reopened = Db::open(&path).unwrap();
        let items = reopened.list_qbit_rss_items().unwrap();
        assert_eq!(items.len(), 16);
        assert_eq!(items["feed-15"]["index"], 15);
    }

    #[test]
    fn qbit_rss_item_json_has_an_aggregate_byte_limit() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let long_text = "\0".repeat(8_192);

        let error = db
            .update_qbit_rss_items(|items| {
                for index in 0..64 {
                    items.insert(
                        format!("feed-{index}"),
                        serde_json::json!({
                            "type": "feed",
                            "uid": long_text,
                            "name": long_text,
                            "url": long_text,
                            "articles": [],
                        }),
                    );
                }
            })
            .expect_err("oversized aggregate RSS state must not persist");

        assert!(error.is::<QbitRssItemCapacityError>());
        assert!(db.get_kv(QBIT_RSS_ITEMS_KEY).unwrap().is_none());
    }

    #[test]
    fn rss_rule_ingress_is_capped_and_qbit_upserts_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(&directory.path().join("cache.sqlite")).unwrap();
        let rules = (0..MAX_RSS_RULES)
            .map(|index| RssRule {
                id: format!("rule-{index}"),
                name: format!("Rule {index}"),
                enabled: true,
                feed_url: "https://example.invalid/rss".to_owned(),
                include: "linux".to_owned(),
                exclude: None,
                category: None,
                save_path: None,
                tags: Vec::new(),
                start: true,
            })
            .collect::<Vec<_>>();
        db.set_kv(RSS_KEY, &serde_json::to_string(&rules).unwrap())
            .unwrap();

        let replacement = RssRule {
            id: String::new(),
            name: "Rule 0".to_owned(),
            enabled: true,
            feed_url: "https://example.invalid/rss".to_owned(),
            include: "updated".to_owned(),
            exclude: None,
            category: None,
            save_path: None,
            tags: Vec::new(),
            start: true,
        };
        let RssRuleUpsertResult::Upserted(updated) = db
            .upsert_qbit_rss_rule(replacement)
            .expect("existing named qBit rule remains updateable at capacity")
        else {
            panic!("existing qBit rule was rejected at capacity");
        };
        assert_eq!(updated.len(), MAX_RSS_RULES);
        assert_eq!(updated[0].id, "rule-0");
        assert_eq!(updated[0].include, "updated");

        let new_rule = RssRule {
            id: String::new(),
            name: "New Rule".to_owned(),
            enabled: true,
            feed_url: "https://example.invalid/rss".to_owned(),
            include: "linux".to_owned(),
            exclude: None,
            category: None,
            save_path: None,
            tags: Vec::new(),
            start: true,
        };
        assert_eq!(
            db.upsert_qbit_rss_rule(new_rule.clone()).unwrap(),
            RssRuleUpsertResult::Capacity
        );
        assert_eq!(
            db.upsert_rss_rule(new_rule).unwrap(),
            RssRuleUpsertResult::Capacity
        );
        assert_eq!(db.list_rss_rules().unwrap().len(), MAX_RSS_RULES);
    }
}
