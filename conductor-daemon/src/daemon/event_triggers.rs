// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

/// Event trigger engine (R915-R917)
///
/// Evaluates trigger conditions against the monitor event stream and fires
/// actions (log messages, desktop notifications) when conditions are met.
use conductor_core::config::types::{EventTrigger, TriggerAction};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Parsed trigger condition
#[derive(Debug, Clone)]
pub struct ParsedCondition {
    /// Event type to count (e.g., "action_error" for error_rate)
    pub event_type: String,
    /// Comparison operator
    pub op: CompareOp,
    /// Threshold value
    pub threshold: u64,
    /// Time window for evaluation
    pub window: Duration,
}

#[derive(Debug, Clone, Copy)]
pub enum CompareOp {
    GreaterThan,
    LessThan,
    GreaterOrEqual,
    LessOrEqual,
}

/// Active trigger with parsed condition and state
struct ActiveTrigger {
    name: String,
    condition: ParsedCondition,
    action: TriggerAction,
    cooldown: Duration,
    last_fired: Option<Instant>,
    /// Rolling window of matching event timestamps
    event_times: VecDeque<Instant>,
}

pub struct TriggerEngine {
    triggers: Vec<ActiveTrigger>,
}

impl TriggerEngine {
    pub fn new(config: &BTreeMap<String, EventTrigger>) -> Self {
        let triggers = config
            .iter()
            .filter_map(
                |(name, trigger)| match parse_condition(&trigger.condition) {
                    Some(condition) => Some(ActiveTrigger {
                        name: name.clone(),
                        condition,
                        action: trigger.action.clone(),
                        cooldown: Duration::from_secs(trigger.cooldown_secs.unwrap_or(60)),
                        last_fired: None,
                        event_times: VecDeque::new(),
                    }),
                    None => {
                        warn!(
                            trigger = %name,
                            condition = %trigger.condition,
                            "Failed to parse trigger condition — skipping"
                        );
                        None
                    }
                },
            )
            .collect();

        Self { triggers }
    }

    /// Check an event against all triggers. Returns any fired actions.
    pub fn check_event(&mut self, event_type: &str) -> Vec<FiredTrigger> {
        let now = Instant::now();
        let mut fired = Vec::new();

        for trigger in &mut self.triggers {
            // Always prune stale events, even for non-matching event types,
            // to prevent memory leaks from accumulated timestamps.
            if let Some(cutoff) = now.checked_sub(trigger.condition.window) {
                while trigger.event_times.front().is_some_and(|&t| t < cutoff) {
                    trigger.event_times.pop_front();
                }
            }

            // Only count events matching the trigger's event type
            // "*" matches all events
            if trigger.condition.event_type != "*" && event_type != trigger.condition.event_type {
                continue;
            }

            trigger.event_times.push_back(now);

            // Cap deque size to prevent unbounded growth during event floods
            while trigger.event_times.len() > 10_000 {
                trigger.event_times.pop_front();
            }

            let count = trigger.event_times.len() as u64;
            let threshold_met = match trigger.condition.op {
                CompareOp::GreaterThan => count > trigger.condition.threshold,
                CompareOp::LessThan => count < trigger.condition.threshold,
                CompareOp::GreaterOrEqual => count >= trigger.condition.threshold,
                CompareOp::LessOrEqual => count <= trigger.condition.threshold,
            };

            if threshold_met {
                // Check cooldown
                if let Some(last) = trigger.last_fired
                    && now.duration_since(last) < trigger.cooldown
                {
                    continue;
                }
                trigger.last_fired = Some(now);
                info!(
                    trigger = %trigger.name,
                    count = count,
                    "Event trigger fired"
                );
                fired.push(FiredTrigger {
                    name: trigger.name.clone(),
                    action: trigger.action.clone(),
                    count,
                });
            }
        }

        fired
    }

    /// Returns true if there are any active triggers
    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty()
    }
}

/// A trigger that has fired
pub struct FiredTrigger {
    pub name: String,
    pub action: TriggerAction,
    pub count: u64,
}

/// Parse a condition string like "error_rate > 5 per_minute"
fn parse_condition(condition: &str) -> Option<ParsedCondition> {
    let parts: Vec<&str> = condition.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }

    // Map metric names to event types
    let event_type = match parts[0] {
        "error_rate" | "errors" => "action_error".to_string(),
        "event_count" | "events" => "*".to_string(), // any event
        other => other.to_string(),                  // literal event_type
    };

    let op = match parts[1] {
        ">" => CompareOp::GreaterThan,
        "<" => CompareOp::LessThan,
        ">=" => CompareOp::GreaterOrEqual,
        "<=" => CompareOp::LessOrEqual,
        _ => return None,
    };

    let threshold: u64 = parts[2].parse().ok()?;

    let window = match parts[3] {
        "per_second" => Duration::from_secs(1),
        "per_minute" => Duration::from_secs(60),
        "per_hour" => Duration::from_secs(3600),
        _ => return None,
    };

    Some(ParsedCondition {
        event_type,
        op,
        threshold,
        window,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::TriggerAction;

    #[test]
    fn test_parse_condition_error_rate() {
        let cond = parse_condition("error_rate > 5 per_minute").unwrap();
        assert_eq!(cond.event_type, "action_error");
        assert_eq!(cond.threshold, 5);
        assert!(matches!(cond.op, CompareOp::GreaterThan));
        assert_eq!(cond.window, Duration::from_secs(60));
    }

    #[test]
    fn test_parse_condition_event_count() {
        let cond = parse_condition("event_count >= 100 per_second").unwrap();
        assert_eq!(cond.event_type, "*");
        assert_eq!(cond.threshold, 100);
        assert!(matches!(cond.op, CompareOp::GreaterOrEqual));
    }

    #[test]
    fn test_parse_condition_literal_event_type() {
        let cond = parse_condition("note_on > 50 per_second").unwrap();
        assert_eq!(cond.event_type, "note_on");
    }

    #[test]
    fn test_parse_condition_invalid() {
        assert!(parse_condition("").is_none());
        assert!(parse_condition("error_rate >").is_none());
        assert!(parse_condition("error_rate > abc per_minute").is_none());
        assert!(parse_condition("error_rate > 5 per_millennium").is_none());
    }

    #[test]
    fn test_trigger_fires_on_threshold() {
        let mut config = BTreeMap::new();
        config.insert(
            "high_errors".to_string(),
            EventTrigger {
                condition: "action_error > 2 per_minute".to_string(),
                action: TriggerAction::Log {
                    message: "High error rate!".to_string(),
                },
                cooldown_secs: Some(0), // No cooldown for test
            },
        );

        let mut engine = TriggerEngine::new(&config);

        // First 2 errors — below threshold
        assert!(engine.check_event("action_error").is_empty());
        assert!(engine.check_event("action_error").is_empty());

        // 3rd error — exceeds threshold (> 2)
        let fired = engine.check_event("action_error");
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "high_errors");
    }

    #[test]
    fn test_trigger_ignores_non_matching_events() {
        let mut config = BTreeMap::new();
        config.insert(
            "errors".to_string(),
            EventTrigger {
                condition: "action_error > 1 per_minute".to_string(),
                action: TriggerAction::Notification {
                    message: "Error!".to_string(),
                },
                cooldown_secs: Some(0),
            },
        );

        let mut engine = TriggerEngine::new(&config);
        assert!(engine.check_event("note_on").is_empty());
        assert!(engine.check_event("cc").is_empty());
        assert!(engine.check_event("action_executed").is_empty());
    }

    #[test]
    fn test_trigger_cooldown() {
        let mut config = BTreeMap::new();
        config.insert(
            "errors".to_string(),
            EventTrigger {
                condition: "action_error > 0 per_minute".to_string(),
                action: TriggerAction::Log {
                    message: "Error!".to_string(),
                },
                cooldown_secs: Some(300), // 5 min cooldown
            },
        );

        let mut engine = TriggerEngine::new(&config);

        // First fire
        let fired = engine.check_event("action_error");
        assert_eq!(fired.len(), 1);

        // Second should be suppressed by cooldown
        let fired = engine.check_event("action_error");
        assert!(fired.is_empty());
    }

    #[test]
    fn test_trigger_wildcard_event_count() {
        let mut config = BTreeMap::new();
        config.insert(
            "flood".to_string(),
            EventTrigger {
                condition: "event_count > 3 per_minute".to_string(),
                action: TriggerAction::Log {
                    message: "Event flood!".to_string(),
                },
                cooldown_secs: Some(0),
            },
        );

        let mut engine = TriggerEngine::new(&config);
        assert!(engine.check_event("note_on").is_empty());
        assert!(engine.check_event("cc").is_empty());
        assert!(engine.check_event("encoder").is_empty());
        // 4th event of any type triggers
        let fired = engine.check_event("pitch_bend");
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].name, "flood");
    }

    #[test]
    fn test_trigger_engine_empty() {
        let engine = TriggerEngine::new(&BTreeMap::new());
        assert!(engine.is_empty());
    }

    #[test]
    fn test_non_matching_events_still_prune_stale_timestamps() {
        // Non-matching events must prune old timestamps
        // to prevent memory leaks from accumulated event_times.
        let mut config = BTreeMap::new();
        config.insert(
            "errors".to_string(),
            EventTrigger {
                condition: "action_error > 2 per_second".to_string(),
                action: TriggerAction::Log {
                    message: "Errors!".to_string(),
                },
                cooldown_secs: Some(0),
            },
        );

        let mut engine = TriggerEngine::new(&config);

        // Send matching events to populate the deque
        engine.check_event("action_error");
        engine.check_event("action_error");

        // Verify deque has 2 entries
        assert_eq!(engine.triggers[0].event_times.len(), 2);

        // Wait for the window to expire, then send non-matching events
        std::thread::sleep(Duration::from_millis(1100));
        engine.check_event("note_on"); // non-matching, but should prune

        // Stale timestamps should have been pruned
        assert_eq!(engine.triggers[0].event_times.len(), 0);
    }

    #[test]
    fn test_deque_cap_prevents_unbounded_growth() {
        let mut config = BTreeMap::new();
        config.insert(
            "flood".to_string(),
            EventTrigger {
                condition: "event_count > 999999 per_hour".to_string(),
                action: TriggerAction::Log {
                    message: "Flood!".to_string(),
                },
                cooldown_secs: Some(0),
            },
        );

        let mut engine = TriggerEngine::new(&config);

        // Send more than 10_000 events
        for _ in 0..10_050 {
            engine.check_event("note_on");
        }

        // Deque should be capped at 10_000
        assert!(engine.triggers[0].event_times.len() <= 10_000);
    }
}
