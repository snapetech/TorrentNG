use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use super::db::Db;

const KEY: &str = "workflow_rules";
const RUNS_KEY: &str = "workflow_runs";
const RSS_KEY: &str = "rss_rules";
const MAX_RUNS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub event: String,
    pub action: String,
    pub category: Option<String>,
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
    pub applied: Vec<String>,
    pub errors: Vec<String>,
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
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![KEY], |r| {
                r.get(0)
            })
            .optional()?;
        let mut rules: Vec<WorkflowRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        rules.sort_by_key(|a| a.name.to_lowercase());
        Ok(rules)
    }

    pub fn upsert_workflow_rule(&self, mut rule: WorkflowRule) -> Result<Vec<WorkflowRule>> {
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

    pub fn workflow_hashes(&self, rule: &WorkflowRule) -> Result<Vec<String>> {
        let conn = self.0.lock().expect("db");
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
            args.push(category.clone());
        }
        if let Some(tracker) = &rule.tracker {
            clauses.push(format!(
                "instr(lower(tracker_url), lower(?{})) > 0",
                args.len() + 1
            ));
            args.push(tracker.clone());
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT hash FROM torrents{where_sql} ORDER BY name COLLATE NOCASE"
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
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![RUNS_KEY],
                |r| r.get(0),
            )
            .optional()?;
        let mut runs: Vec<WorkflowRun> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        runs.sort_by_key(|run| {
            (
                std::cmp::Reverse(run.started_at),
                std::cmp::Reverse(run.id.clone()),
            )
        });
        Ok(runs)
    }

    pub fn record_workflow_run(&self, run: WorkflowRun) -> Result<Vec<WorkflowRun>> {
        self.update_workflow_runs(|runs| {
            runs.push(run);
            runs.sort_by_key(|run| {
                (
                    std::cmp::Reverse(run.started_at),
                    std::cmp::Reverse(run.id.clone()),
                )
            });
            runs.truncate(MAX_RUNS);
        })
    }

    pub fn list_rss_rules(&self) -> Result<Vec<RssRule>> {
        let conn = self.0.lock().expect("db");
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

    pub fn upsert_rss_rule(&self, mut rule: RssRule) -> Result<Vec<RssRule>> {
        if rule.id.trim().is_empty() {
            rule.id = uuid::Uuid::new_v4().to_string();
        }
        let id = rule.id.clone();
        self.update_rss_rules(|rules| {
            rules.retain(|existing| existing.id != id);
            rules.push(rule);
        })
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
        let mut conn = self.0.lock().expect("db");
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
        update(&mut rules);
        rules.sort_by_key(|rule| rule.name.to_lowercase());
        write_json_vec(&tx, KEY, &rules)?;
        tx.commit()?;
        Ok(rules)
    }

    fn update_workflow_runs<F>(&self, update: F) -> Result<Vec<WorkflowRun>>
    where
        F: FnOnce(&mut Vec<WorkflowRun>),
    {
        let mut conn = self.0.lock().expect("db");
        let tx = conn.transaction()?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![RUNS_KEY],
                |r| r.get(0),
            )
            .optional()?;
        let mut runs: Vec<WorkflowRun> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        update(&mut runs);
        runs.sort_by_key(|run| {
            (
                std::cmp::Reverse(run.started_at),
                std::cmp::Reverse(run.id.clone()),
            )
        });
        runs.truncate(MAX_RUNS);
        write_json_vec(&tx, RUNS_KEY, &runs)?;
        tx.commit()?;
        Ok(runs)
    }

    fn update_rss_rules<F>(&self, update: F) -> Result<Vec<RssRule>>
    where
        F: FnOnce(&mut Vec<RssRule>),
    {
        let mut conn = self.0.lock().expect("db");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssRuleRenameResult {
    Missing,
    Conflict,
    Renamed,
}

fn write_json_vec<T: Serialize>(tx: &Transaction<'_>, key: &str, values: &[T]) -> Result<()> {
    let raw = serde_json::to_string(values)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread};

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
}
