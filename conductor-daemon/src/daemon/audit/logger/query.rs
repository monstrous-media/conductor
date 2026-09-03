// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Audit log read path: filtered `query` + aggregate `get_summary`.
//! Split out of `logger/mod.rs` so the read path stays a
//! small, independently-reviewable unit. Pure move — behaviour is
//! unchanged; the audit test-suite is the regression guard.

use super::*;

impl AuditLogger {
    /// Build the shared parameterised `WHERE` clause + bound params for the
    /// filterable columns of an `AuditQuery` — tool_name, risk_tier,
    /// event_type, errors_only, start_time, end_time. Used by BOTH `query`
    /// and `get_summary` so summaries honour the same filters as row queries.
    /// Result-shaping options (limit/offset/order) are not part of
    /// this clause. String filters are bound as parameters, never
    /// interpolated, so they can't break out of the query.
    fn build_filter_clause(query: &AuditQuery) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
        let mut clause = String::from("WHERE 1=1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref tool) = query.tool_name {
            clause.push_str(" AND tool_name = ?");
            params.push(Box::new(tool.clone()));
        }
        if let Some(ref tier) = query.risk_tier {
            clause.push_str(" AND risk_tier = ?");
            params.push(Box::new(tier.as_str().to_string()));
        }
        if let Some(ref event) = query.event_type {
            clause.push_str(" AND event_type = ?");
            params.push(Box::new(event.as_str().to_string()));
        }
        if query.errors_only {
            clause.push_str(" AND is_error = 1");
        }
        if let Some(start) = query.start_time {
            clause.push_str(" AND created_at >= ?");
            params.push(Box::new(start));
        }
        if let Some(end) = query.end_time {
            clause.push_str(" AND created_at <= ?");
            params.push(Box::new(end));
        }

        (clause, params)
    }

    /// Query audit entries
    pub fn query(&self, query: &AuditQuery) -> SqliteResult<Vec<AuditEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

        // D4.A.3.3.B.2: `provenance` is the v3 column. v2-era rows
        // load it as NULL; v3+ rows hold a JSON-serialised Provenance.
        let (where_clause, params) = Self::build_filter_clause(query);
        let mut sql = format!(
            "SELECT id, event_type, tool_name, user_context, arguments, result, \
             risk_tier, is_error, error_message, execution_time_ms, created_at, \
             provenance \
             FROM audit_log {where_clause}"
        );

        sql.push_str(" ORDER BY created_at DESC");

        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = query.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;

        let entries = stmt.query_map(param_refs.as_slice(), |row| {
            let event_type_str: String = row.get(1)?;
            let risk_tier_str: String = row.get(6)?;
            let user_context_json: Option<String> = row.get(3)?;
            let execution_time_ms: Option<i64> = row.get(9)?;
            // D4.A.3.3.B.2: deserialise the v3 `provenance` column.
            // NULL → None (v2-era rows + new ReadOnly entries). On a
            // JSON parse error we tolerate and return None rather than
            // failing the entire query — a malformed-provenance entry
            // is recoverable for triage; a hard failure here would
            // mask legitimate audit data behind a single bad row.
            let provenance_json: Option<String> = row.get(11)?;
            let provenance =
                provenance_json.and_then(|s| serde_json::from_str::<Provenance>(&s).ok());

            Ok(AuditEntry {
                id: row.get(0)?,
                event_type: AuditEventType::parse(&event_type_str)
                    .unwrap_or(AuditEventType::ToolComplete),
                tool_name: row.get(2)?,
                user_context: user_context_json.and_then(|s| UserContext::from_json(&s)),
                arguments: row.get(4)?,
                result: row.get(5)?,
                risk_tier: AuditRiskTier::parse(&risk_tier_str).unwrap_or(AuditRiskTier::Internal),
                is_error: row.get::<_, i32>(7)? != 0,
                error_message: row.get(8)?,
                execution_time: execution_time_ms.map(|ms| Duration::from_millis(ms as u64)),
                created_at: row.get(10)?,
                provenance,
            })
        })?;

        entries.collect()
    }

    /// Get summary statistics for the audit log
    pub fn get_summary(&self, query: &AuditQuery) -> SqliteResult<AuditSummary> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

        // Apply the SAME filters as `query` (not just the time
        // window) so a filtered summary reports filtered numbers. Params
        // are bound, so the string filters (tool_name, risk_tier,
        // event_type) are injection-safe.
        let (where_clause, params) = Self::build_filter_clause(query);
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let params = param_refs.as_slice();

        // Total and error counts. `SUM(...)` is NULL over an empty
        // result set (empty log, or a filter that matches nothing), which
        // can't be read as `i64` — `COALESCE(..., 0)` keeps it a 0 count
        // instead of erroring the whole summary.
        let (total_count, error_count): (u64, u64) = conn.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN is_error = 1 THEN 1 ELSE 0 END), 0) \
                 FROM audit_log {}",
                where_clause
            ),
            params,
            // rusqlite 0.40 dropped the blanket FromSql for u64; COUNT/SUM are
            // non-negative, so read as i64 and widen.
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )?;

        // Average execution time
        let avg_execution_time_ms: Option<f64> = conn.query_row(
            &format!(
                "SELECT AVG(execution_time_ms) FROM audit_log {} AND execution_time_ms IS NOT NULL",
                where_clause
            ),
            params,
            |row| row.get(0),
        ).ok();

        // By risk tier
        let mut by_risk_tier = std::collections::HashMap::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT risk_tier, COUNT(*) FROM audit_log {} GROUP BY risk_tier",
            where_clause
        ))?;
        let rows = stmt.query_map(params, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        for (tier, count) in rows.flatten() {
            by_risk_tier.insert(tier, count);
        }

        // By tool name (top 10)
        let mut by_tool_name = std::collections::HashMap::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT tool_name, COUNT(*) as cnt FROM audit_log {} AND tool_name IS NOT NULL \
             GROUP BY tool_name ORDER BY cnt DESC LIMIT 10",
            where_clause
        ))?;
        let rows = stmt.query_map(params, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        for (name, count) in rows.flatten() {
            by_tool_name.insert(name, count);
        }

        // Time range
        let time_range: Option<(i64, i64)> = conn
            .query_row(
                &format!(
                    "SELECT MIN(created_at), MAX(created_at) FROM audit_log {}",
                    where_clause
                ),
                params,
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        Ok(AuditSummary {
            total_count,
            error_count,
            by_risk_tier,
            by_tool_name,
            avg_execution_time_ms,
            time_range,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_by_risk_tier() {
        let logger = AuditLogger::in_memory().unwrap();

        // Log some entries with different tiers
        logger.log_tool_complete(
            "conductor_get_config",
            AuditRiskTier::ReadOnly,
            None,
            None,
            Duration::from_millis(10),
            None,
        );
        logger.log_tool_complete(
            "conductor_update_mode",
            AuditRiskTier::ConfigChange,
            None,
            None,
            Duration::from_millis(20),
            None,
        );
        logger.log_tool_complete(
            "conductor_learn_start",
            AuditRiskTier::Stateful,
            None,
            None,
            Duration::from_millis(15),
            None,
        );

        // Query only ConfigChange
        let entries = logger
            .query(&AuditQuery {
                risk_tier: Some(AuditRiskTier::ConfigChange),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].tool_name,
            Some("conductor_update_mode".to_string())
        );
    }

    #[test]
    fn test_get_summary() {
        let logger = AuditLogger::in_memory().unwrap();

        // Log several entries
        for i in 0..5 {
            logger.log_tool_complete(
                "conductor_get_config",
                AuditRiskTier::ReadOnly,
                None,
                None,
                Duration::from_millis(10 + i),
                None,
            );
        }
        logger.log_tool_error(
            "conductor_update_mode",
            AuditRiskTier::ConfigChange,
            None,
            "Error",
            Duration::from_millis(5),
            None,
        );

        let summary = logger.get_summary(&AuditQuery::default()).unwrap();

        assert_eq!(summary.total_count, 6);
        assert_eq!(summary.error_count, 1);
        assert_eq!(*summary.by_risk_tier.get("read_only").unwrap_or(&0), 5);
        assert_eq!(*summary.by_risk_tier.get("config_change").unwrap_or(&0), 1);
    }

    /// `get_summary` must honour the same filters as `query`
    /// (tool_name, risk_tier, event_type, errors_only), not just the time
    /// window. Pre-fix it only applied start/end_time, so a filtered
    /// summary returned whole-log numbers.
    #[test]
    fn test_get_summary_applies_query_filters() {
        let logger = AuditLogger::in_memory().unwrap();

        // 3 ReadOnly successes on "get_config".
        for _ in 0..3 {
            logger.log_tool_complete(
                "get_config",
                AuditRiskTier::ReadOnly,
                None,
                None,
                Duration::from_millis(10),
                None,
            );
        }
        // 1 ConfigChange error on "update_mode".
        logger.log_tool_error(
            "update_mode",
            AuditRiskTier::ConfigChange,
            None,
            "boom",
            Duration::from_millis(5),
            None,
        );

        // Whole-log baseline.
        assert_eq!(
            logger
                .get_summary(&AuditQuery::default())
                .unwrap()
                .total_count,
            4
        );

        // risk_tier filter → only the ConfigChange error.
        let by_tier = logger
            .get_summary(&AuditQuery {
                risk_tier: Some(AuditRiskTier::ConfigChange),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            by_tier.total_count, 1,
            "risk_tier filter must scope summary"
        );
        assert_eq!(by_tier.error_count, 1);

        // errors_only filter → only the 1 error.
        assert_eq!(
            logger
                .get_summary(&AuditQuery {
                    errors_only: true,
                    ..Default::default()
                })
                .unwrap()
                .total_count,
            1,
            "errors_only filter must scope summary"
        );

        // tool_name filter → only the 3 get_config rows.
        let by_tool = logger
            .get_summary(&AuditQuery {
                tool_name: Some("get_config".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            by_tool.total_count, 3,
            "tool_name filter must scope summary"
        );
        assert_eq!(by_tool.error_count, 0);

        // event_type filter → only ToolComplete rows (the 3 successes).
        assert_eq!(
            logger
                .get_summary(&AuditQuery {
                    event_type: Some(AuditEventType::ToolComplete),
                    ..Default::default()
                })
                .unwrap()
                .total_count,
            3,
            "event_type filter must scope summary"
        );
    }

    /// `get_summary` must not error on an empty result set. With no
    /// rows, `SUM(...)` is NULL; without `COALESCE` it fails to parse as
    /// i64. Covers both an empty log and a filter that matches nothing.
    #[test]
    fn test_get_summary_on_empty_result_set() {
        let logger = AuditLogger::in_memory().unwrap();

        // Empty log.
        let empty = logger.get_summary(&AuditQuery::default()).unwrap();
        assert_eq!(empty.total_count, 0);
        assert_eq!(empty.error_count, 0);

        // Non-empty log, but a filter that matches nothing.
        logger.log_tool_complete(
            "get_config",
            AuditRiskTier::ReadOnly,
            None,
            None,
            Duration::from_millis(10),
            None,
        );
        let no_match = logger
            .get_summary(&AuditQuery {
                tool_name: Some("does_not_exist".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(no_match.total_count, 0);
        assert_eq!(no_match.error_count, 0);
    }
}
