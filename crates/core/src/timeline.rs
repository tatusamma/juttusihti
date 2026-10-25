//! Timeline data: articles and events in a time range, with story clusters
//! derived from non-rejected relationships. Publication times and dates
//! described by content (events) are kept apart; nothing is guessed.

use std::collections::HashMap;

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::search::{SearchQuery, build_where};
use crate::store::Store;
use crate::{ArticleId, Result, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineArticle {
    pub id: ArticleId,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    /// Publication time, or fetch time when `dated` is false.
    pub at: i64,
    pub dated: bool,
    /// Story cluster (smallest article id of its linked component), if linked.
    pub cluster: Option<ArticleId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEvent {
    pub id: i64,
    pub title: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub status: Status,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    pub articles: Vec<TimelineArticle>,
    /// Events whose described date falls in the range.
    pub events: Vec<TimelineEvent>,
    /// Events linked to these articles that have no known date.
    pub undated_events: Vec<TimelineEvent>,
    pub truncated: bool,
}

fn find(parent: &mut HashMap<i64, i64>, x: i64) -> i64 {
    let p = *parent.get(&x).unwrap_or(&x);
    if p == x {
        return x;
    }
    let root = find(parent, p);
    parent.insert(x, root);
    root
}

impl Store {
    /// Articles matching `q` (newest first, up to `limit`), plus related
    /// events and story clusters.
    pub fn timeline(&self, q: &SearchQuery, limit: u32) -> Result<Timeline> {
        let (from_where, mut params, _) = build_where(q)?;
        params.push(Value::Integer(i64::from(limit) + 1));
        let mut stmt = self.conn().prepare(&format!(
            "SELECT a.id, a.feed_id, f.title, a.title, a.effective_at, a.published_at IS NOT NULL
             {from_where} ORDER BY a.effective_at DESC, a.id DESC LIMIT ?"
        ))?;
        let mut articles: Vec<TimelineArticle> = stmt
            .query_map(params_from_iter(params.iter()), |r| {
                Ok(TimelineArticle {
                    id: r.get(0)?,
                    feed_id: r.get(1)?,
                    feed_title: r.get(2)?,
                    title: r.get(3)?,
                    at: r.get(4)?,
                    dated: r.get(5)?,
                    cluster: None,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let truncated = articles.len() > limit as usize;
        articles.truncate(limit as usize);
        articles.reverse(); // oldest first for drawing

        let ids: std::collections::HashSet<i64> = articles.iter().map(|a| a.id).collect();
        let mut parent: HashMap<i64, i64> = HashMap::new();
        let mut rel = self.conn().prepare_cached(
            "SELECT source_id, target_id FROM article_relationships
             WHERE (source_id = ?1 OR target_id = ?1) AND status <> 'rejected'",
        )?;
        for a in &articles {
            let pairs = rel.query_map([a.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            for p in pairs {
                let (s, t) = p?;
                if ids.contains(&s) && ids.contains(&t) {
                    // Register both ends so every member, including the root, is found later.
                    parent.entry(s).or_insert(s);
                    parent.entry(t).or_insert(t);
                    let (rs, rt) = (find(&mut parent, s), find(&mut parent, t));
                    if rs != rt {
                        parent.insert(rs.max(rt), rs.min(rt));
                    }
                }
            }
        }
        for a in &mut articles {
            if parent.contains_key(&a.id) {
                a.cluster = Some(find(&mut parent, a.id));
            }
        }

        let mut ev = self.conn().prepare_cached(
            "SELECT DISTINCT e.id, e.title, e.starts_at, e.ends_at, e.status FROM events e
             JOIN article_events ae ON ae.event_id = e.id
             WHERE ae.article_id = ?1 AND e.status <> 'rejected' AND ae.status <> 'rejected'",
        )?;
        let mut seen = std::collections::HashSet::new();
        let (mut events, mut undated_events) = (Vec::new(), Vec::new());
        for a in &articles {
            let rows = ev.query_map([a.id], |r| {
                Ok(TimelineEvent {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    starts_at: r.get(2)?,
                    ends_at: r.get(3)?,
                    status: r.get(4)?,
                })
            })?;
            for e in rows {
                let e = e?;
                if !seen.insert(e.id) {
                    continue;
                }
                match e.starts_at {
                    None => undated_events.push(e),
                    Some(t) if q.from.is_none_or(|f| t >= f) && q.to.is_none_or(|u| t < u) => events.push(e),
                    // Dated outside the requested range: listed with undated so it is not lost.
                    Some(_) => undated_events.push(e),
                }
            }
        }
        events.sort_by_key(|e| e.starts_at);
        Ok(Timeline { articles, events, undated_events, truncated })
    }
}
