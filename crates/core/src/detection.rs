use std::collections::HashSet;

use serde_json::Value;

use crate::QueryResult;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detection {
    /// Complete current baseline in stable serialized order.
    pub current_keys: Vec<String>,
    pub new_key_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum DetectionError {
    #[error("at least one identity column must be selected")]
    MissingIdentity,
    #[error("identity column {0} is not in the query result")]
    MissingColumn(String),
    #[error("identity column {0} contains NULL or is absent")]
    NullIdentity(String),
    #[error("query returned a duplicate composite identity")]
    DuplicateIdentity,
    #[error("could not serialize a row identity: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Compare complete key sets. Keys that disappeared on a previous successful
/// execution and return now are new again. Non-key values are ignored.
///
/// # Errors
/// Fails on missing/null identities or duplicate keys rather than corrupting
/// the baseline or silently suppressing a future event.
pub fn evaluate(
    result: &QueryResult,
    key_columns: &[String],
    previous: &[String],
    initialized: bool,
    trigger_on_first_run: bool,
) -> Result<Detection, DetectionError> {
    if key_columns.is_empty() {
        return Err(DetectionError::MissingIdentity);
    }
    for column in key_columns {
        if !result.columns.iter().any(|name| name == column) {
            return Err(DetectionError::MissingColumn(column.clone()));
        }
    }
    let old: HashSet<&str> = previous.iter().map(String::as_str).collect();
    let mut current = HashSet::new();
    let mut newly_observed: usize = 0;
    for row in &result.rows {
        let mut values = Vec::<Value>::with_capacity(key_columns.len());
        for column in key_columns {
            let value = row
                .get(column)
                .ok_or_else(|| DetectionError::NullIdentity(column.clone()))?;
            if value.is_null() {
                return Err(DetectionError::NullIdentity(column.clone()));
            }
            values.push(value.clone());
        }
        let key = serde_json::to_string(&values)?;
        if !current.insert(key.clone()) {
            return Err(DetectionError::DuplicateIdentity);
        }
        if (initialized || trigger_on_first_run) && !old.contains(key.as_str()) {
            newly_observed = newly_observed.saturating_add(1);
        }
    }
    let mut current_keys: Vec<_> = current.into_iter().collect();
    current_keys.sort_unstable();
    Ok(Detection {
        current_keys,
        new_key_count: newly_observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn result(ids: &[(&str, i64)]) -> QueryResult {
        let rows = ids
            .iter()
            .map(|(id, amount)| {
                let mut row = Map::new();
                row.insert("ID".to_owned(), json!(id));
                row.insert("AMOUNT".to_owned(), json!(amount));
                row
            })
            .collect();
        QueryResult {
            columns: vec!["ID".to_owned(), "AMOUNT".to_owned()],
            rows,
        }
    }

    fn keys() -> Vec<String> {
        vec!["ID".to_owned()]
    }

    #[test]
    fn first_run_baseline_default() {
        let observation = evaluate(&result(&[("A", 1)]), &keys(), &[], false, false);
        assert!(
            observation
                .as_ref()
                .is_ok_and(|value| value.new_key_count == 0 && value.current_keys.len() == 1),
            "{observation:?}"
        );
    }

    fn detection_sequence() -> Result<(usize, usize, usize), DetectionError> {
        let a = evaluate(&result(&[("A", 1), ("B", 3)]), &keys(), &[], false, true)?;
        let b = evaluate(&result(&[("B", 4)]), &keys(), &a.current_keys, true, false)?;
        let c = evaluate(
            &result(&[("A", 8), ("B", 99)]),
            &keys(),
            &b.current_keys,
            true,
            false,
        )?;
        Ok((a.new_key_count, b.new_key_count, c.new_key_count))
    }

    #[test]
    fn detects_new_and_returning_keys_but_ignores_value_changes() {
        let sizes = detection_sequence();
        assert!(
            sizes.as_ref().is_ok_and(|value| *value == (2, 0, 1)),
            "{sizes:?}"
        );
    }

    #[test]
    fn failed_identity_validation_does_not_produce_a_baseline() {
        let bad = result(&[("A", 1), ("A", 2)]);
        assert!(matches!(
            evaluate(&bad, &keys(), &[], true, true),
            Err(DetectionError::DuplicateIdentity)
        ));
    }

    fn empty_baseline_sequence() -> Result<usize, DetectionError> {
        let a = evaluate(&result(&[]), &keys(), &[], false, false)?;
        let b = evaluate(&result(&[("A", 5)]), &keys(), &a.current_keys, true, false)?;
        Ok(b.new_key_count)
    }

    #[test]
    fn empty_initialized_baseline_is_distinct_from_first_run() {
        let count = empty_baseline_sequence();
        assert!(count.as_ref().is_ok_and(|value| *value == 1), "{count:?}");
    }
}
