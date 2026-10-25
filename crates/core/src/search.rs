//! Local full-text search over SQLite FTS5.
//!
//! User (or AI-suggested) query text is parsed into a small AST and then
//! rendered into FTS5 syntax with every term quoted, so input can never
//! inject FTS5 operators or SQL. Filters become bound SQL parameters.
//!
//! Supported syntax:
//! - `word`, `word*` (prefix), `"exact phrase"`
//! - `AND` (also implicit between terms), `OR`, `NOT x` / `-x`
//! - `( ... )` grouping
//! - `NEAR(word word ..., 10)` proximity
//! - `title:word`, `author:word`, `summary:word`, `content:word` column filters

use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

use crate::{ArticleSummary, Error, Page, Result, View};

/// Snippet highlight markers. The UI splits on these; they never occur in
/// stored text because `html_to_text` output does not contain control chars
/// that matter here and the markers are stripped on input.
pub const HIGHLIGHT_START: char = '\u{2}';
pub const HIGHLIGHT_END: char = '\u{3}';

const MAX_QUERY_CHARS: usize = 2_000;
const MAX_NODES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Column {
    Title,
    Summary,
    Content,
    Author,
}

impl Column {
    fn fts_name(&self) -> &'static str {
        match self {
            Column::Title => "title",
            Column::Summary => "summary_text",
            Column::Content => "content_text",
            Column::Author => "author",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "title" | "otsikko" => Some(Column::Title),
            "summary" | "description" => Some(Column::Summary),
            "content" | "text" => Some(Column::Content),
            "author" => Some(Column::Author),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    Term { text: String, prefix: bool, column: Option<Column> },
    Phrase { words: Vec<String>, column: Option<Column> },
    Near { words: Vec<String>, distance: u32 },
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

/// Parses query text. Returns `Ok(None)` for an empty query.
pub fn parse_query(input: &str) -> Result<Option<Node>> {
    if input.chars().count() > MAX_QUERY_CHARS {
        return Err(Error::Query("query is too long".into()));
    }
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut p = Parser { tokens, pos: 0, nodes: 0 };
    let node = p.or()?;
    if p.pos != p.tokens.len() {
        return Err(Error::Query(format!("unexpected {:?}", p.tokens[p.pos])));
    }
    validate(&node, true)?;
    Ok(Some(node))
}

/// Renders a parsed query as an FTS5 MATCH expression.
pub fn to_fts5(node: &Node) -> String {
    match node {
        Node::Term { text, prefix, column } => {
            let mut s = col_prefix(column);
            s.push_str(&quote(text));
            if *prefix {
                s.push('*');
            }
            s
        }
        Node::Phrase { words, column } => {
            let mut s = col_prefix(column);
            s.push_str(&quote(&words.join(" ")));
            s
        }
        Node::Near { words, distance } => {
            let inner: Vec<String> = words.iter().map(|w| quote(w)).collect();
            format!("NEAR({}, {distance})", inner.join(" "))
        }
        Node::Or(items) => {
            let parts: Vec<String> = items.iter().map(to_fts5).collect();
            format!("({})", parts.join(" OR "))
        }
        Node::And(items) => {
            let (neg, pos): (Vec<&Node>, Vec<&Node>) = items.iter().partition(|n| matches!(n, Node::Not(_)));
            let pos: Vec<String> = pos.into_iter().map(to_fts5).collect();
            let mut s = format!("({})", pos.join(" AND "));
            for n in neg {
                if let Node::Not(inner) = n {
                    s = format!("({s} NOT {})", to_fts5(inner));
                }
            }
            s
        }
        // A standalone NOT is rejected by `validate`; render defensively.
        Node::Not(inner) => to_fts5(inner),
    }
}

/// Human-readable form of the interpreted query, shown to the user.
pub fn describe(node: &Node) -> String {
    match node {
        Node::Term { text, prefix, column } => {
            format!(
                "{}{}{}",
                column.as_ref().map(|c| format!("{}:", c.fts_name())).unwrap_or_default(),
                text,
                if *prefix { "*" } else { "" }
            )
        }
        Node::Phrase { words, column } => {
            format!(
                "{}\"{}\"",
                column.as_ref().map(|c| format!("{}:", c.fts_name())).unwrap_or_default(),
                words.join(" ")
            )
        }
        Node::Near { words, distance } => format!("NEAR({}, {distance})", words.join(" ")),
        Node::And(items) => format!("({})", items.iter().map(describe).collect::<Vec<_>>().join(" AND ")),
        Node::Or(items) => format!("({})", items.iter().map(describe).collect::<Vec<_>>().join(" OR ")),
        Node::Not(inner) => format!("NOT {}", describe(inner)),
    }
}

fn col_prefix(column: &Option<Column>) -> String {
    column.as_ref().map(|c| format!("{} : ", c.fts_name())).unwrap_or_default()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn validate(node: &Node, top: bool) -> Result<()> {
    match node {
        Node::Not(_) if top => Err(Error::Query("a query cannot consist only of excluded terms".into())),
        Node::Not(inner) => validate(inner, false),
        Node::And(items) => {
            if items.iter().all(|n| matches!(n, Node::Not(_))) {
                return Err(Error::Query("a query needs at least one term that is not excluded".into()));
            }
            items.iter().try_for_each(|n| validate(n, false))
        }
        Node::Or(items) => items.iter().try_for_each(|n| match n {
            Node::Not(_) => Err(Error::Query("NOT cannot be used directly inside OR".into())),
            n => validate(n, false),
        }),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    Phrase(String),
    Column(Column),
    LParen,
    RParen,
    Comma,
    And,
    Or,
    Not,
    Near,
    Star,
}

fn tokenize(input: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let mut chars = input.chars().filter(|c| *c != HIGHLIGHT_START && *c != HIGHLIGHT_END).peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut phrase = String::new();
            let mut closed = false;
            for ch in chars.by_ref() {
                if ch == '"' {
                    closed = true;
                    break;
                }
                phrase.push(ch);
            }
            if !closed {
                return Err(Error::Query("unterminated quote".into()));
            }
            out.push(Tok::Phrase(phrase));
        } else if c == '(' {
            chars.next();
            out.push(Tok::LParen);
        } else if c == ')' {
            chars.next();
            out.push(Tok::RParen);
        } else if c == ',' {
            chars.next();
            out.push(Tok::Comma);
        } else if c == '*' {
            chars.next();
            out.push(Tok::Star);
        } else if c == '-' {
            // A leading '-' negates the following term; stray dashes are ignored.
            chars.next();
            if chars.peek().is_some_and(|n| n.is_alphanumeric() || *n == '"' || *n == '(') {
                out.push(Tok::Not);
            }
        } else {
            let mut word = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || matches!(ch, '"' | '(' | ')' | ',' | '*') {
                    break;
                }
                if ch == ':' {
                    break;
                }
                word.push(ch);
                chars.next();
            }
            if chars.peek() == Some(&':') {
                chars.next();
                match Column::parse(&word) {
                    Some(col) => {
                        out.push(Tok::Column(col));
                        continue;
                    }
                    None => word.push(':'),
                }
            }
            match word.as_str() {
                "AND" => out.push(Tok::And),
                "OR" => out.push(Tok::Or),
                "NOT" => out.push(Tok::Not),
                "NEAR" if chars.peek() == Some(&'(') => out.push(Tok::Near),
                _ => {
                    // Keep only characters the unicode61 tokenizer treats as
                    // token characters; split the rest into separate words.
                    let parts: Vec<String> = split_token_chars(&word);
                    match parts.len() {
                        0 => {}
                        1 => out.push(Tok::Word(parts.into_iter().next().unwrap())),
                        // "Pohjois-Suomi" style words become an exact phrase.
                        _ => out.push(Tok::Phrase(parts.join(" "))),
                    }
                }
            }
        }
    }
    Ok(out)
}

fn split_token_chars(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).map(str::to_string).collect()
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
    nodes: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn count(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(Error::Query("query has too many terms".into()));
        }
        Ok(())
    }

    fn or(&mut self) -> Result<Node> {
        let mut items = vec![self.and()?];
        while self.peek() == Some(&Tok::Or) {
            self.bump();
            items.push(self.and()?);
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Node::Or(items) })
    }

    fn and(&mut self) -> Result<Node> {
        let mut items = vec![self.unary()?];
        loop {
            match self.peek() {
                Some(Tok::And) => {
                    self.bump();
                    items.push(self.unary()?);
                }
                Some(Tok::Or | Tok::RParen | Tok::Comma) | None => break,
                Some(_) => items.push(self.unary()?),
            }
        }
        Ok(if items.len() == 1 { items.pop().unwrap() } else { Node::And(items) })
    }

    fn unary(&mut self) -> Result<Node> {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            return Ok(Node::Not(Box::new(self.primary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Node> {
        self.count()?;
        let column = if let Some(Tok::Column(c)) = self.peek() {
            let c = c.clone();
            self.bump();
            Some(c)
        } else {
            None
        };
        match self.bump() {
            Some(Tok::Word(w)) => {
                let prefix = self.peek() == Some(&Tok::Star);
                if prefix {
                    self.bump();
                }
                Ok(Node::Term { text: w, prefix, column })
            }
            Some(Tok::Phrase(p)) => {
                let words = split_token_chars(&p);
                if words.is_empty() {
                    return Err(Error::Query("empty phrase".into()));
                }
                Ok(Node::Phrase { words, column })
            }
            Some(Tok::LParen) if column.is_none() => {
                let inner = self.or()?;
                if self.bump() != Some(Tok::RParen) {
                    return Err(Error::Query("missing closing parenthesis".into()));
                }
                Ok(inner)
            }
            Some(Tok::Near) if column.is_none() => self.near(),
            Some(t) => Err(Error::Query(format!("unexpected {t:?}"))),
            None => Err(Error::Query("query ends unexpectedly".into())),
        }
    }

    fn near(&mut self) -> Result<Node> {
        if self.bump() != Some(Tok::LParen) {
            return Err(Error::Query("NEAR must be followed by '('".into()));
        }
        let mut words = Vec::new();
        let mut distance = 10;
        loop {
            match self.bump() {
                Some(Tok::Word(w)) => words.push(w),
                Some(Tok::Phrase(p)) => words.extend(split_token_chars(&p)),
                Some(Tok::Comma) => {
                    match self.bump() {
                        Some(Tok::Word(n)) => {
                            distance = n.parse().map_err(|_| Error::Query("NEAR distance must be a number".into()))?;
                        }
                        _ => return Err(Error::Query("NEAR distance must be a number".into())),
                    }
                    if self.bump() != Some(Tok::RParen) {
                        return Err(Error::Query("missing ')' after NEAR distance".into()));
                    }
                    break;
                }
                Some(Tok::RParen) => break,
                _ => return Err(Error::Query("NEAR accepts only words".into())),
            }
        }
        if words.len() < 2 {
            return Err(Error::Query("NEAR needs at least two words".into()));
        }
        Ok(Node::Near { words, distance: distance.min(1000) })
    }
}

/// A complete, serializable search: text plus structured filters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub view: View,
    pub feed_ids: Vec<i64>,
    pub folder_ids: Vec<i64>,
    /// Inclusive lower bound, UTC unix seconds.
    pub from: Option<i64>,
    /// Exclusive upper bound, UTC unix seconds.
    pub to: Option<i64>,
    /// Articles must carry all of these labels.
    pub label_ids: Vec<i64>,
    pub entity_ids: Vec<i64>,
    pub event_ids: Vec<i64>,
    /// Also match AI suggestions that the user has not yet confirmed.
    pub include_suggested: bool,
    /// Only articles whose feed supplied a publication date. When false,
    /// undated articles fall back to their fetch time for date ranges.
    #[serde(default)]
    pub require_published_date: bool,
    /// Hide articles the user confirmed as unrelated, duplicate, low
    /// information or clickbait. Turning this off reveals them again.
    #[serde(default)]
    pub hide_dismissed: bool,
}

/// Search results plus the interpreted query for display.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResults {
    pub items: Vec<ArticleSummary>,
    pub total: u64,
    pub interpreted: Option<String>,
}

/// Builds the WHERE clause shared by search, counting and analysis scoping.
/// Returns (`FROM ... WHERE ...` SQL, parameters, has_fts).
pub(crate) fn build_where(q: &SearchQuery) -> Result<(String, Vec<Value>, Option<Node>)> {
    let node = parse_query(&q.text)?;
    let mut sql = String::from(" FROM articles a JOIN feeds f ON f.id = a.feed_id");
    let mut params: Vec<Value> = Vec::new();
    if node.is_some() {
        sql.push_str(" JOIN articles_fts ON articles_fts.rowid = a.id");
    }
    sql.push_str(" WHERE 1=1");
    if let Some(n) = &node {
        sql.push_str(" AND articles_fts MATCH ?");
        params.push(Value::Text(to_fts5(n)));
    }
    match q.view {
        View::All => {}
        View::Unread => sql.push_str(" AND a.is_read = 0"),
        View::Saved => sql.push_str(" AND a.is_saved = 1"),
    }
    if q.require_published_date {
        sql.push_str(" AND a.published_at IS NOT NULL");
    }
    if q.hide_dismissed {
        sql.push_str(&format!(
            " AND NOT EXISTS (SELECT 1 FROM article_relevance rv WHERE rv.article_id = a.id
               AND rv.status = 'confirmed' AND rv.class IN {})",
            crate::review::DISMISSED_CLASSES
        ));
    }
    let in_list = |sql: &mut String, params: &mut Vec<Value>, col: &str, ids: &[i64]| {
        if !ids.is_empty() {
            sql.push_str(&format!(" AND {col} IN ({})", vec!["?"; ids.len()].join(",")));
            params.extend(ids.iter().map(|i| Value::Integer(*i)));
        }
    };
    in_list(&mut sql, &mut params, "a.feed_id", &q.feed_ids);
    in_list(&mut sql, &mut params, "f.folder_id", &q.folder_ids);
    if let Some(from) = q.from {
        sql.push_str(" AND a.effective_at >= ?");
        params.push(Value::Integer(from));
    }
    if let Some(to) = q.to {
        sql.push_str(" AND a.effective_at < ?");
        params.push(Value::Integer(to));
    }
    let statuses = if q.include_suggested { "('confirmed','suggested')" } else { "('confirmed')" };
    for id in &q.label_ids {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM article_labels x WHERE x.article_id = a.id AND x.label_id = ? AND x.status IN {statuses})"
        ));
        params.push(Value::Integer(*id));
    }
    for id in &q.entity_ids {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM article_entities x WHERE x.article_id = a.id AND x.entity_id = ? AND x.status IN {statuses})"
        ));
        params.push(Value::Integer(*id));
    }
    for id in &q.event_ids {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM article_events x WHERE x.article_id = a.id AND x.event_id = ? AND x.status IN {statuses})"
        ));
        params.push(Value::Integer(*id));
    }
    Ok((sql, params, node))
}

/// Runs a search with pagination. Text queries are ranked by bm25 with
/// title matches weighted highest; filter-only queries are newest first.
pub fn search(conn: &rusqlite::Connection, q: &SearchQuery, page: Page) -> Result<SearchResults> {
    let (from_where, mut params, node) = build_where(q)?;
    let total: i64 =
        conn.query_row(&format!("SELECT count(*){from_where}"), rusqlite::params_from_iter(params.iter()), |r| {
            r.get(0)
        })?;
    let (select_extra, order) = if node.is_some() {
        (
            format!(
                ", snippet(articles_fts, -1, '{HIGHLIGHT_START}', '{HIGHLIGHT_END}', '…', 16), bm25(articles_fts, 10.0, 3.0, 1.0, 2.0)"
            ),
            " ORDER BY bm25(articles_fts, 10.0, 3.0, 1.0, 2.0), a.effective_at DESC",
        )
    } else {
        (", NULL, NULL".to_string(), " ORDER BY a.effective_at DESC, a.id DESC")
    };
    let sql = format!(
        "SELECT a.id, a.feed_id, f.title, a.title, a.url, a.effective_at, a.is_read, a.is_saved{select_extra}{from_where}{order} LIMIT ? OFFSET ?"
    );
    params.push(Value::Integer(page.limit as i64));
    params.push(Value::Integer(page.offset as i64));
    let mut stmt = conn.prepare(&sql)?;
    let items = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(ArticleSummary {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                title: r.get(3)?,
                url: r.get(4)?,
                effective_at: r.get(5)?,
                is_read: r.get(6)?,
                is_saved: r.get(7)?,
                snippet: r.get(8)?,
                rank: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(SearchResults { items, total: total as u64, interpreted: node.as_ref().map(describe) })
}

/// Article ids matching a query, newest first, up to `limit`. Used to
/// scope analysis sessions.
pub fn matching_ids(conn: &rusqlite::Connection, q: &SearchQuery, limit: u32) -> Result<Vec<i64>> {
    let (from_where, mut params, _) = build_where(q)?;
    params.push(Value::Integer(limit as i64));
    let mut stmt = conn.prepare(&format!("SELECT a.id{from_where} ORDER BY a.effective_at DESC, a.id DESC LIMIT ?"))?;
    let ids = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fts(q: &str) -> String {
        to_fts5(&parse_query(q).unwrap().unwrap())
    }

    #[test]
    fn renders_basic_syntax() {
        assert_eq!(fts("sää"), "\"sää\"");
        assert_eq!(fts("työttömyys*"), "\"työttömyys\"*");
        assert_eq!(fts("a b"), "(\"a\" AND \"b\")");
        assert_eq!(fts("a OR b c"), "(\"a\" OR (\"b\" AND \"c\"))");
        assert_eq!(fts("\"city council\""), "\"city council\"");
        assert_eq!(fts("title:tram"), "title : \"tram\"");
        assert_eq!(fts("NEAR(council tram, 5)"), "NEAR(\"council\" \"tram\", 5)");
        assert_eq!(fts("(a OR b) NOT c"), "(((\"a\" OR \"b\")) NOT \"c\")");
        assert_eq!(fts("a -b"), "((\"a\") NOT \"b\")");
        assert_eq!(fts("Pohjois-Suomi"), "\"Pohjois Suomi\"");
    }

    #[test]
    fn neutralizes_operator_injection() {
        // FTS5 special characters inside words never reach the MATCH string unquoted.
        assert_eq!(fts("a^b"), "\"a b\"");
        assert_eq!(fts("x:y"), "(\"x\" AND \"y\")");
        assert_eq!(fts("near"), "\"near\"");
        assert_eq!(fts("'; DROP TABLE articles; --"), "(\"DROP\" AND \"TABLE\" AND \"articles\")");
    }

    #[test]
    fn rejects_bad_queries() {
        for q in ["NOT a", "-a", "\"open", "(a", "a OR NOT b", "NEAR(a)", "NEAR(a b, x)"] {
            assert!(parse_query(q).is_err(), "{q} should be rejected");
        }
        assert!(parse_query("   ").unwrap().is_none());
        assert!(parse_query(&"a ".repeat(100)).is_err());
    }

    #[test]
    fn describes_interpretation() {
        let n = parse_query("title:sää OR lumi*").unwrap().unwrap();
        assert_eq!(describe(&n), "(title:sää OR lumi*)");
    }
}
