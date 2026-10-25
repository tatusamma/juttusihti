//! Relationship graph: a bounded neighbourhood of articles, entities and
//! events around chosen seeds, plus a deterministic force-directed layout.
//!
//! The graph is built from stored, reviewable data only; it never calls an
//! AI service. Node count is capped so the canvas never draws an unbounded
//! graph, and rejected links are never shown.

use std::collections::{HashMap, VecDeque};

use rusqlite::params;

use crate::store::Store;
use crate::{Origin, RelationshipKind, Result, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeRef {
    Article(i64),
    Entity(i64),
    Event(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: NodeRef,
    pub label: String,
    /// Feed title for articles, kind for entities, "event" for events.
    pub detail: String,
    pub time: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Link(RelationshipKind),
    Mentions,
    InEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdge {
    pub a: usize,
    pub b: usize,
    pub kind: EdgeKind,
    pub origin: Origin,
    pub status: Status,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// More nodes were reachable than `max_nodes` allowed.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphQuery {
    pub seeds: Vec<NodeRef>,
    pub hops: u8,
    pub include_suggested: bool,
    pub include_entities: bool,
    /// Only these relationship kinds (all when empty).
    pub kinds: Vec<RelationshipKind>,
    pub max_nodes: usize,
}

impl Default for GraphQuery {
    fn default() -> Self {
        Self {
            seeds: Vec::new(),
            hops: 2,
            include_suggested: true,
            include_entities: true,
            kinds: Vec::new(),
            max_nodes: 150,
        }
    }
}

/// Neighbours fetched per node, so one popular entity cannot flood the graph.
const PER_NODE: i64 = 40;

struct Builder<'a> {
    store: &'a Store,
    q: &'a GraphQuery,
    index: HashMap<NodeRef, usize>,
    graph: Graph,
}

impl Builder<'_> {
    fn statuses(&self) -> &'static str {
        if self.q.include_suggested { "('confirmed','suggested')" } else { "('confirmed')" }
    }

    /// Adds a node if there is room; returns its index.
    fn node(&mut self, id: NodeRef) -> Result<Option<usize>> {
        if let Some(i) = self.index.get(&id) {
            return Ok(Some(*i));
        }
        if self.graph.nodes.len() >= self.q.max_nodes {
            self.graph.truncated = true;
            return Ok(None);
        }
        let conn = self.store.conn();
        let node = match id {
            NodeRef::Article(a) => conn.query_row(
                "SELECT a.title, f.title, a.effective_at FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
                [a],
                |r| Ok(GraphNode { id, label: r.get(0)?, detail: r.get(1)?, time: r.get(2)? }),
            ),
            NodeRef::Entity(e) => conn.query_row("SELECT name, kind FROM entities WHERE id = ?1", [e], |r| {
                Ok(GraphNode { id, label: r.get(0)?, detail: r.get(1)?, time: None })
            }),
            NodeRef::Event(e) => conn.query_row("SELECT title, starts_at FROM events WHERE id = ?1", [e], |r| {
                Ok(GraphNode { id, label: r.get(0)?, detail: "event".into(), time: r.get(1)? })
            }),
        };
        let node = match node {
            Ok(n) => n,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        self.graph.nodes.push(node);
        let i = self.graph.nodes.len() - 1;
        self.index.insert(id, i);
        Ok(Some(i))
    }

    fn edge(&mut self, a: usize, b: usize, kind: EdgeKind, origin: Origin, status: Status, confidence: Option<f64>) {
        let dup = self.graph.edges.iter().any(|e| e.kind == kind && ((e.a, e.b) == (a, b) || (e.a, e.b) == (b, a)));
        if a != b && !dup {
            self.graph.edges.push(GraphEdge { a, b, kind, origin, status, confidence });
        }
    }

    /// Neighbours of one node as (neighbour, edge kind, origin, status, confidence).
    #[allow(clippy::type_complexity)]
    fn neighbours(&self, id: NodeRef) -> Result<Vec<(NodeRef, EdgeKind, Origin, Status, Option<f64>)>> {
        let conn = self.store.conn();
        let st = self.statuses();
        let mut out = Vec::new();
        match id {
            NodeRef::Article(a) => {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT CASE WHEN source_id = ?1 THEN target_id ELSE source_id END, kind, origin, status, confidence
                     FROM article_relationships WHERE (source_id = ?1 OR target_id = ?1) AND status IN {st}
                     ORDER BY status = 'confirmed' DESC, confidence DESC LIMIT ?2"
                ))?;
                let rows = stmt.query_map(params![a, PER_NODE], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, RelationshipKind>(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?;
                for row in rows {
                    let (other, kind, origin, status, conf) = row?;
                    if self.q.kinds.is_empty() || self.q.kinds.contains(&kind) {
                        out.push((NodeRef::Article(other), EdgeKind::Link(kind), origin, status, conf));
                    }
                }
                if self.q.include_entities {
                    let mut stmt = conn.prepare_cached(&format!(
                        "SELECT entity_id, origin, status FROM article_entities WHERE article_id = ?1 AND status IN {st} LIMIT ?2"
                    ))?;
                    let rows =
                        stmt.query_map(params![a, PER_NODE], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
                    for row in rows {
                        let (e, origin, status) = row?;
                        out.push((NodeRef::Entity(e), EdgeKind::Mentions, origin, status, None));
                    }
                }
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT event_id, origin, status FROM article_events WHERE article_id = ?1 AND status IN {st} LIMIT ?2"
                ))?;
                let rows = stmt.query_map(params![a, PER_NODE], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
                for row in rows {
                    let (e, origin, status) = row?;
                    out.push((NodeRef::Event(e), EdgeKind::InEvent, origin, status, None));
                }
            }
            NodeRef::Entity(e) => {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT ae.article_id, ae.origin, ae.status FROM article_entities ae JOIN articles a ON a.id = ae.article_id
                     WHERE ae.entity_id = ?1 AND ae.status IN {st} ORDER BY a.effective_at DESC LIMIT ?2"
                ))?;
                let rows = stmt.query_map(params![e, PER_NODE], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
                for row in rows {
                    let (a, origin, status) = row?;
                    out.push((NodeRef::Article(a), EdgeKind::Mentions, origin, status, None));
                }
            }
            NodeRef::Event(ev) => {
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT article_id, origin, status FROM article_events WHERE event_id = ?1 AND status IN {st} LIMIT ?2"
                ))?;
                let rows =
                    stmt.query_map(params![ev, PER_NODE], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))?;
                for row in rows {
                    let (a, origin, status) = row?;
                    out.push((NodeRef::Article(a), EdgeKind::InEvent, origin, status, None));
                }
            }
        }
        Ok(out)
    }
}

impl Store {
    /// Builds the neighbourhood graph around the query's seeds.
    pub fn graph(&self, q: &GraphQuery) -> Result<Graph> {
        let mut b = Builder { store: self, q, index: HashMap::new(), graph: Graph::default() };
        let mut queue: VecDeque<(NodeRef, u8)> = VecDeque::new();
        for s in &q.seeds {
            if b.node(*s)?.is_some() {
                queue.push_back((*s, 0));
            }
        }
        while let Some((id, depth)) = queue.pop_front() {
            if depth >= q.hops {
                continue;
            }
            let from = b.index[&id];
            for (other, kind, origin, status, conf) in b.neighbours(id)? {
                let known = b.index.contains_key(&other);
                let Some(to) = b.node(other)? else { continue };
                b.edge(from, to, kind, origin, status, conf);
                if !known {
                    queue.push_back((other, depth + 1));
                }
            }
        }
        Ok(b.graph)
    }

    /// Articles of a session that take part in at least one non-rejected
    /// relationship, as graph seeds.
    pub fn session_graph_seeds(&self, session: i64, limit: u32) -> Result<Vec<NodeRef>> {
        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT sa.article_id FROM analysis_session_articles sa
             JOIN article_relationships r ON (r.source_id = sa.article_id OR r.target_id = sa.article_id)
             WHERE sa.session_id = ?1 AND r.status <> 'rejected' LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session, limit], |r| Ok(NodeRef::Article(r.get(0)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Entities whose name or alias contains `text` (case-insensitive).
    pub fn find_entities(&self, text: &str, limit: u32) -> Result<Vec<crate::Entity>> {
        let needle = format!("%{}%", crate::normalize::normalize_text(text));
        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT e.id, e.kind, e.name FROM entities e LEFT JOIN entity_aliases a ON a.entity_id = e.id
             WHERE e.normalized LIKE ?1 OR a.normalized LIKE ?1 ORDER BY e.name COLLATE NOCASE LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![needle, limit], |r| {
            Ok(crate::Entity { id: r.get(0)?, kind: r.get(1)?, name: r.get(2)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Fruchterman–Reingold layout in the unit square [-1, 1]². Deterministic:
/// the same graph always gets the same picture.
pub fn layout(n: usize, edges: &[(usize, usize)], iterations: usize) -> Vec<(f32, f32)> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![(0.0, 0.0)];
    }
    // Start on a circle (golden-angle spiral) for determinism.
    let mut pos: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let r = ((i as f32 + 0.5) / n as f32).sqrt();
            let a = i as f32 * 2.399_963;
            (r * a.cos(), r * a.sin())
        })
        .collect();
    let k = (4.0 / n as f32).sqrt();
    let mut temp = 0.2_f32;
    let cool = temp / (iterations.max(1) as f32 + 1.0);
    let mut disp = vec![(0.0_f32, 0.0_f32); n];
    for _ in 0..iterations {
        disp.iter_mut().for_each(|d| *d = (0.0, 0.0));
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (pos[i].0 - pos[j].0, pos[i].1 - pos[j].1);
                let d = (dx * dx + dy * dy).sqrt().max(1e-4);
                let f = k * k / d;
                let (fx, fy) = (dx / d * f, dy / d * f);
                disp[i].0 += fx;
                disp[i].1 += fy;
                disp[j].0 -= fx;
                disp[j].1 -= fy;
            }
        }
        for &(a, b) in edges {
            if a >= n || b >= n || a == b {
                continue;
            }
            let (dx, dy) = (pos[a].0 - pos[b].0, pos[a].1 - pos[b].1);
            let d = (dx * dx + dy * dy).sqrt().max(1e-4);
            let f = d * d / k;
            let (fx, fy) = (dx / d * f, dy / d * f);
            disp[a].0 -= fx;
            disp[a].1 -= fy;
            disp[b].0 += fx;
            disp[b].1 += fy;
        }
        for i in 0..n {
            let (dx, dy) = disp[i];
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            let step = len.min(temp);
            pos[i].0 = (pos[i].0 + dx / len * step).clamp(-1.0, 1.0);
            pos[i].1 = (pos[i].1 + dy / len * step).clamp(-1.0, 1.0);
        }
        temp = (temp - cool).max(0.002);
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_finite_bounded_and_deterministic() {
        let edges = vec![(0, 1), (1, 2), (2, 0), (3, 4)];
        let a = layout(6, &edges, 200);
        let b = layout(6, &edges, 200);
        assert_eq!(a, b);
        assert!(a.iter().all(|(x, y)| x.is_finite() && y.is_finite() && x.abs() <= 1.0 && y.abs() <= 1.0));
        let dist = |i: usize, j: usize| ((a[i].0 - a[j].0).powi(2) + (a[i].1 - a[j].1).powi(2)).sqrt();
        let connected = (dist(0, 1) + dist(1, 2) + dist(3, 4)) / 3.0;
        let apart = (dist(0, 3) + dist(2, 4) + dist(1, 5)) / 3.0;
        assert!(connected < apart, "linked nodes end up closer ({connected} vs {apart})");
        assert_eq!(layout(0, &[], 10), vec![]);
        assert_eq!(layout(1, &[], 10), vec![(0.0, 0.0)]);
    }
}
