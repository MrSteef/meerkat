//! Application-level SQL guard. A least-privileged read-only Oracle account and
//! a read-only transaction remain the final enforcement layer.
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, Query, SetExpr, Statement, Value, Visit, Visitor};
use sqlparser::dialect::OracleDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};

#[derive(Default)]
struct CheckQuery;

impl Visitor for CheckQuery {
    type Break = String;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if !query.locks.is_empty() {
            return ControlFlow::Break("SELECT FOR UPDATE is not allowed".to_owned());
        }
        if matches!(
            query.body.as_ref(),
            SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_)
        ) {
            return ControlFlow::Break("modifying query bodies are not allowed".to_owned());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Value(value) = expr
            && matches!(&value.value, Value::Placeholder(_))
        {
            return ControlFlow::Break("bind placeholders are not supported in V1".to_owned());
        }
        ControlFlow::Continue(())
    }
}

/// Accepts exactly one SQL SELECT (including SELECT with CTEs), with no binds
/// or FOR UPDATE. Literal semicolons in quoted strings are fine.
///
/// # Errors
/// Returns a diagnostic for invalid or unsafe query syntax.
pub fn validate_read_only_sql(sql: &str) -> Result<(), String> {
    let tokens = Tokenizer::new(&OracleDialect, sql)
        .tokenize()
        .map_err(|error| error.to_string())?;
    let words: Vec<_> = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .collect();
    if words.is_empty() {
        return Err("SQL query cannot be empty".to_owned());
    }
    if words.iter().any(|token| matches!(token, Token::SemiColon)) {
        return Err("SQL must be a single statement without a semicolon".to_owned());
    }
    // A WITH FUNCTION block can contain PL/SQL: never allow it.
    if words
        .first()
        .is_some_and(|token| token.to_string().eq_ignore_ascii_case("WITH"))
        && words.get(1).is_some_and(|token| {
            matches!(
                token.to_string().to_ascii_uppercase().as_str(),
                "FUNCTION" | "PROCEDURE"
            )
        })
    {
        return Err("WITH FUNCTION/PROCEDURE is not supported".to_owned());
    }
    let parsed = Parser::parse_sql(&OracleDialect, sql).map_err(|error| error.to_string())?;
    let [Statement::Query(query)] = parsed.as_slice() else {
        return Err("only a single SELECT query is allowed".to_owned());
    };
    if let ControlFlow::Break(error) = query.visit(&mut CheckQuery) {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_wms_queries() {
        assert!(validate_read_only_sql("SELECT 1 FROM dual").is_ok());
        assert!(
            validate_read_only_sql("WITH active AS (SELECT 1 FROM dual) SELECT * FROM active")
                .is_ok()
        );
        assert!(validate_read_only_sql("SELECT ';' FROM dual").is_ok());
    }
    #[test]
    fn rejects_mutating_queries() {
        for sql in [
            "DELETE FROM waves",
            "SELECT 1 FROM dual;",
            "SELECT * FROM waves FOR UPDATE",
            "SELECT :x FROM dual",
        ] {
            assert!(
                validate_read_only_sql(sql).is_err(),
                "query should have been rejected: {sql}"
            );
        }
    }
}
