//! チームタブの関係図: 点 = エージェント(と、つながりのあるメイン)、線 = 実際に取れた関係だけ。
//! 線になるのは (1) 親が子を起こした関係(エージェントの parentId / parentThreadId / parent、
//! 無ければ path の親)と (2) メッセージ板の送信元と宛先。どちらの端も点に結べないものは、
//! 推測で補わず、引かない。

use crate::{AppWindow, GraphEdge, GraphNode, TeamState};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// この時間より新しいメッセージがある関係を「指示中」として動かす。
const ACTIVE_MS: i64 = 30_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub label: String,
    pub detail: String,
    pub state: String,
    pub main: bool,
    pub x: f32,
    pub y: f32,
    pub dy: f32,
    pub span: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub active: bool,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub levels: usize,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn last_segment(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

fn is_main(p: &str) -> bool {
    let t = p.trim_end_matches('/');
    t == "/root" || t == "root"
}

fn parent_path(p: &str) -> Option<String> {
    let t = p.trim_end_matches('/');
    let i = t.rfind('/')?;
    if i == 0 {
        None
    } else {
        Some(t[..i].to_string())
    }
}

const MAIN: usize = usize::MAX;

struct Agent<'a> {
    v: &'a Value,
}

impl Agent<'_> {
    fn path(&self) -> &str {
        let p = s(self.v, "path");
        if p.is_empty() { s(self.v, "agentPath") } else { p }
    }
}

/// A name from the server (a path, an id or a thread id) -> an agent, or the main thread.
fn resolve(agents: &[Agent], name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    if is_main(name) {
        return Some(MAIN);
    }
    // the exact names first
    for (i, a) in agents.iter().enumerate() {
        if a.path() == name || s(a.v, "id") == name || s(a.v, "threadId") == name {
            return Some(i);
        }
    }
    // then the last part of a path against the name the agent was given
    if name.contains('/') {
        let seg = last_segment(name);
        if !seg.is_empty() {
            for (i, a) in agents.iter().enumerate() {
                if s(a.v, "name") == seg || s(a.v, "nickname") == seg || s(a.v, "id") == seg {
                    return Some(i);
                }
            }
        }
    }
    None
}

fn parent_of(agents: &[Agent], i: usize) -> Option<usize> {
    let a = &agents[i];
    for k in ["parentId", "parentThreadId", "parent"] {
        let v = s(a.v, k);
        if !v.is_empty() {
            return resolve(agents, v).filter(|&p| p != i);
        }
    }
    let path = a.path();
    if path.is_empty() {
        return None;
    }
    parent_path(path).and_then(|pp| resolve(agents, &pp)).filter(|&p| p != i)
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn state_label(state: &str) -> &str {
    match state {
        "running" => "実行中",
        "waiting" => "待機中",
        "blocked" => "ブロック中",
        "done" => "完了",
        other => other,
    }
}

pub fn build(agent_values: &[Value], messages: &[Value], now: i64) -> Graph {
    let agents: Vec<Agent> = agent_values.iter().map(|v| Agent { v }).collect();
    let n = agents.len();

    // relations as pairs of (from, to) with MAIN for the main thread; the flag says "a parent edge"
    let mut parent: Vec<Option<usize>> = (0..n).map(|i| parent_of(&agents, i)).collect();
    let mut pairs: Vec<(usize, usize, bool, i64)> = Vec::new();
    for (i, p) in parent.iter().enumerate() {
        if let Some(p) = p {
            pairs.push((*p, i, true, 0));
        }
    }
    for m in messages {
        let (Some(a), Some(b)) = (resolve(&agents, s(m, "from")), resolve(&agents, s(m, "to"))) else { continue };
        if a == b {
            continue;
        }
        let ts = m.get("tsMs").and_then(Value::as_i64).unwrap_or(0);
        pairs.push((a, b, false, ts));
    }
    if pairs.is_empty() {
        return Graph::default();
    }

    // the main thread is a point only when a relation reaches it
    let main_idx = if pairs.iter().any(|p| p.0 == MAIN || p.1 == MAIN) { Some(n) } else { None };
    let fix = |x: usize| if x == MAIN { n } else { x };

    // the points that have a relation; the others are in the grid and the list below
    let mut used = vec![false; n + 1];
    for p in &pairs {
        used[fix(p.0)] = true;
        used[fix(p.1)] = true;
    }
    parent.push(None);
    let total = n + 1;

    // levels by the parent relation
    let mut level = vec![0usize; total];
    for _ in 0..total {
        let mut changed = false;
        for i in 0..n {
            if let Some(p) = parent[i] {
                let want = level[fix(p)] + 1;
                if level[i] < want && want <= total {
                    level[i] = want;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let indices: Vec<usize> = (0..total).filter(|&i| used[i] && (i != n || main_idx.is_some())).collect();
    let levels = indices.iter().map(|&i| level[i]).max().unwrap_or(0) + 1;

    // order inside a level by where the parent stands; x spread evenly
    let mut x = vec![0.5f32; total];
    let mut span = vec![1.0f32; total];
    let mut dy = vec![0.0f32; total];
    for lv in 0..levels {
        let mut row: Vec<usize> = indices.iter().copied().filter(|&i| level[i] == lv).collect();
        row.sort_by(|&a, &b| {
            let pa = if a < n { parent[a].map(|p| x[fix(p)]).unwrap_or(0.0) } else { 0.0 };
            let pb = if b < n { parent[b].map(|p| x[fix(p)]).unwrap_or(0.0) } else { 0.0 };
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
        });
        let count = row.len() as f32;
        for (k, &i) in row.iter().enumerate() {
            x[i] = (k as f32 + 0.5) / count;
            span[i] = 1.0 / count * if row.len() > 7 { 2.0 } else { 1.0 };
            // a crowded level zigzags so that the names do not lie on each other
            dy[i] = if row.len() > 7 && k % 2 == 1 { 16.0 } else { 0.0 };
        }
    }

    // the nodes in the order of indices; map from the old number to the new
    let mut new_of = vec![usize::MAX; total];
    let mut nodes = Vec::new();
    for &i in &indices {
        new_of[i] = nodes.len();
        let y = if levels > 1 { level[i] as f32 / (levels - 1) as f32 } else { 0.5 };
        if i == n {
            nodes.push(Node { label: "メイン".into(), detail: "メイン".into(), state: String::new(), main: true, x: x[i], y, dy: dy[i], span: span[i] });
        } else {
            let a = &agents[i];
            let name = if s(a.v, "nickname").is_empty() { s(a.v, "name") } else { s(a.v, "nickname") };
            let state = s(a.v, "state");
            let tokens = a.v.get("tokens").and_then(Value::as_i64).map(|t| format!("  {t} トークン")).unwrap_or_default();
            nodes.push(Node {
                label: name.to_string(),
                detail: format!("{}  {}{}", s(a.v, "name"), state_label(state), tokens),
                state: state.to_string(),
                main: false,
                x: x[i],
                y,
                dy: dy[i],
                span: span[i],
            });
        }
    }

    // one line for a pair of points; it moves when a message passed on it lately
    let mut edges: Vec<(usize, usize, i64)> = Vec::new(); // (from, to, last message ts)
    for (a, b, _is_parent, ts) in &pairs {
        let (a, b) = (new_of[fix(*a)], new_of[fix(*b)]);
        match edges.iter_mut().find(|e| (e.0 == a && e.1 == b) || (e.0 == b && e.1 == a)) {
            Some(e) => {
                if *ts > e.2 {
                    *e = (a, b, *ts);
                }
            }
            None => edges.push((a, b, *ts)),
        }
    }
    let edges = edges
        .into_iter()
        .map(|(from, to, ts)| {
            // both ends done: nothing is being said any more
            let both_done = nodes[from].state == "done" && nodes[to].state == "done";
            let active = ts > 0 && now - ts <= ACTIVE_MS && now - ts >= -ACTIVE_MS && !both_done;
            Edge { from, to, active }
        })
        .collect();

    Graph { nodes, edges, levels }
}

/// Shows the graph when there are lines to draw; with none the tab shows no graph.
pub fn render(app: &AppWindow) {
    let agents = super::team::agents_snapshot();
    let messages = super::team_log::messages_snapshot();
    let g = build(&agents, &messages, now_ms());
    let ts = app.global::<TeamState>();
    if g.nodes.len() < 2 || g.edges.is_empty() {
        ts.set_graph_nodes(ModelRc::from(Rc::new(VecModel::from(Vec::<GraphNode>::new()))));
        ts.set_graph_edges(ModelRc::from(Rc::new(VecModel::from(Vec::<GraphEdge>::new()))));
        ts.set_graph_height(0.0);
        return;
    }
    let nodes: Vec<GraphNode> = g
        .nodes
        .iter()
        .map(|n| GraphNode {
            label: n.label.clone().into(),
            detail: n.detail.clone().into(),
            state: n.state.clone().into(),
            main: n.main,
            x: n.x,
            y: n.y,
            dy: n.dy,
            span: n.span,
        })
        .collect();
    let edges: Vec<GraphEdge> = g
        .edges
        .iter()
        .map(|e| {
            let (a, b) = (&g.nodes[e.from], &g.nodes[e.to]);
            GraphEdge { x1: a.x, y1: a.y, dy1: a.dy, x2: b.x, y2: b.y, dy2: b.dy, active: e.active }
        })
        .collect();
    ts.set_graph_nodes(ModelRc::from(Rc::new(VecModel::from(nodes))));
    ts.set_graph_edges(ModelRc::from(Rc::new(VecModel::from(edges))));
    ts.set_graph_height((g.levels as f32 * 64.0 + 56.0).max(120.0));
}
