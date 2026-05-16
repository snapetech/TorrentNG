use anyhow::Result;
use rusqlite::params;
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
            .ok();
        let mut rules: Vec<WorkflowRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        rules.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(rules)
    }

    pub fn upsert_workflow_rule(&self, mut rule: WorkflowRule) -> Result<Vec<WorkflowRule>> {
        if rule.id.trim().is_empty() {
            rule.id = uuid::Uuid::new_v4().to_string();
        }
        let mut rules = self.list_workflow_rules()?;
        rules.retain(|existing| existing.id != rule.id);
        rules.push(rule);
        rules.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_workflow_rules(&rules)?;
        Ok(rules)
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
            clauses.push(format!("tracker_url LIKE ?{}", args.len() + 1));
            args.push(format!("%{tracker}%"));
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
        let mut rules = self.list_workflow_rules()?;
        rules.retain(|existing| existing.id != id);
        self.save_workflow_rules(&rules)?;
        Ok(rules)
    }

    pub fn list_workflow_runs(&self) -> Result<Vec<WorkflowRun>> {
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key=?1",
                params![RUNS_KEY],
                |r| r.get(0),
            )
            .ok();
        let mut runs: Vec<WorkflowRun> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(runs)
    }

    pub fn record_workflow_run(&self, run: WorkflowRun) -> Result<Vec<WorkflowRun>> {
        let mut runs = self.list_workflow_runs()?;
        runs.push(run);
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        runs.truncate(MAX_RUNS);
        self.save_workflow_runs(&runs)?;
        Ok(runs)
    }

    pub fn list_rss_rules(&self) -> Result<Vec<RssRule>> {
        let conn = self.0.lock().expect("db");
        let raw: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key=?1", params![RSS_KEY], |r| {
                r.get(0)
            })
            .ok();
        let mut rules: Vec<RssRule> = match raw {
            Some(raw) => serde_json::from_str(&raw)?,
            None => Vec::new(),
        };
        rules.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(rules)
    }

    pub fn upsert_rss_rule(&self, mut rule: RssRule) -> Result<Vec<RssRule>> {
        if rule.id.trim().is_empty() {
            rule.id = uuid::Uuid::new_v4().to_string();
        }
        let mut rules = self.list_rss_rules()?;
        rules.retain(|existing| existing.id != rule.id);
        rules.push(rule);
        rules.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_rss_rules(&rules)?;
        Ok(rules)
    }

    pub fn delete_rss_rule(&self, id: &str) -> Result<Vec<RssRule>> {
        let mut rules = self.list_rss_rules()?;
        rules.retain(|existing| existing.id != id);
        self.save_rss_rules(&rules)?;
        Ok(rules)
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

    fn save_workflow_rules(&self, rules: &[WorkflowRule]) -> Result<()> {
        let raw = serde_json::to_string(rules)?;
        self.0.lock().expect("db").execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![KEY, raw],
        )?;
        Ok(())
    }

    fn save_workflow_runs(&self, runs: &[WorkflowRun]) -> Result<()> {
        let raw = serde_json::to_string(runs)?;
        self.0.lock().expect("db").execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![RUNS_KEY, raw],
        )?;
        Ok(())
    }

    fn save_rss_rules(&self, rules: &[RssRule]) -> Result<()> {
        let raw = serde_json::to_string(rules)?;
        self.0.lock().expect("db").execute(
            "INSERT INTO kv(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![RSS_KEY, raw],
        )?;
        Ok(())
    }
}

fn pattern_list_matches(patterns: &str, haystack: &str) -> bool {
    patterns
        .split([',', '\n'])
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| haystack.contains(&pattern.to_lowercase()))
}
