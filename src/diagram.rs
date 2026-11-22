//! Diagrams in Markdown: ```mermaid blocks drawn natively, the way plans,
//! agents and reviews use them. Two kinds, the ones plans need:
//!
//! - **Flowcharts** (`flowchart TD|LR|BT|RL`, or `graph …`): nodes in their
//!   shapes (box, rounded, stadium, circle, diamond, database), edges solid,
//!   dotted or thick, with or without arrows and labels, chains (`A --> B --> C`),
//!   `&`, and subgraphs drawn as labelled groups. Laid out in layers: each
//!   node one layer below the nodes it comes from, ordered to cross less.
//! - **Sequence diagrams** (`sequenceDiagram`): participants and actors,
//!   messages (solid or dashed, to oneself too), notes, and `loop`/`alt`/
//!   `opt`/`par`/`critical` frames with `else`.
//!
//! Anything else (or a block that doesn't parse) is shown as code, as before.
//! Styling lines (`classDef`, `style`, `click`, …) are skipped: the diagram
//! takes Monaged's theme.

use std::collections::HashMap;

use gpui::{AnyElement, Hsla, PathBuilder, Pixels, Point, canvas, div, point, prelude::*, px, rgb};

use crate::theme;

// ---- The model ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Down,
    Up,
    Right,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Box,
    Round,
    Stadium,
    Circle,
    Diamond,
    Database,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub dashed: bool,
    pub thick: bool,
    pub arrow: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub title: String,
    pub members: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub dir: Dir,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Message { from: usize, to: usize, text: String, dashed: bool, arrow: bool },
    Note { over: Vec<usize>, text: String },
    /// `loop`, `alt`, `opt`, … opening a frame (kind, label).
    Open(String, String),
    /// `else` / `and` inside a frame.
    Divide(String),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    /// (id, label)
    pub actors: Vec<(String, String)>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Diagram {
    Flow(Flow),
    Sequence(Sequence),
}

/// More nodes than this aren't drawn (the layout stops being readable).
const MAX_NODES: usize = 60;

pub fn parse(code: &str) -> Option<Diagram> {
    let lines: Vec<String> = code
        .lines()
        .map(|l| match l.find("%%") {
            Some(i) => l[..i].trim().to_string(),
            None => l.trim().to_string(),
        })
        .filter(|l| !l.is_empty())
        .collect();
    let first = lines.first()?;
    let head = first.split_whitespace().next()?.to_ascii_lowercase();
    match head.as_str() {
        "flowchart" | "graph" => parse_flow(&lines).filter(|f| !f.nodes.is_empty() && f.nodes.len() <= MAX_NODES).map(Diagram::Flow),
        "sequencediagram" => parse_sequence(&lines[1..]).filter(|s| !s.actors.is_empty()).map(Diagram::Sequence),
        _ => None,
    }
}

// ---- Flowcharts -----------------------------------------------------------------

fn parse_flow(lines: &[String]) -> Option<Flow> {
    let dir = match lines[0].split_whitespace().nth(1).map(|d| d.trim_end_matches(';').to_ascii_uppercase()).as_deref() {
        Some("LR") => Dir::Right,
        Some("RL") => Dir::Left,
        Some("BT") => Dir::Up,
        _ => Dir::Down,
    };
    let mut flow = Flow { dir, nodes: Vec::new(), edges: Vec::new(), groups: Vec::new() };
    let mut open_groups: Vec<usize> = Vec::new();
    for line in &lines[1..] {
        for statement in line.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let lower = statement.to_ascii_lowercase();
            let word = lower.split_whitespace().next().unwrap_or_default();
            match word {
                "subgraph" => {
                    let rest = statement["subgraph".len()..].trim();
                    let title = match (rest.find('['), rest.rfind(']')) {
                        (Some(a), Some(b)) if b > a => clean_label(&rest[a + 1..b]),
                        _ => clean_label(rest),
                    };
                    flow.groups.push(Group { title, members: Vec::new() });
                    open_groups.push(flow.groups.len() - 1);
                }
                "end" => {
                    open_groups.pop();
                }
                "direction" | "classdef" | "class" | "style" | "linkstyle" | "click" | "accTitle" | "acctitle" | "accdescr" => {}
                _ => {
                    let before = flow.nodes.len();
                    parse_chain(statement, &mut flow);
                    if let Some(&g) = open_groups.last() {
                        for n in before..flow.nodes.len() {
                            flow.groups[g].members.push(n);
                        }
                        // Nodes named inside a subgraph belong to it even when declared earlier.
                        if before == flow.nodes.len()
                            && let Some(idx) = node_ids_in(statement).into_iter().find_map(|id| flow.nodes.iter().position(|n| n.id == id))
                            && !flow.groups[g].members.contains(&idx)
                        {
                            flow.groups[g].members.push(idx);
                        }
                    }
                }
            }
        }
    }
    flow.groups.retain(|g| !g.members.is_empty());
    Some(flow)
}

fn node_ids_in(statement: &str) -> Vec<String> {
    let mut at = 0;
    let chars: Vec<char> = statement.chars().collect();
    read_id(&chars, &mut at).into_iter().collect()
}

fn clean_label(text: &str) -> String {
    let t = text.trim().trim_matches('"').trim_matches('`').trim();
    t.replace("<br/>", "\n").replace("<br>", "\n").replace("<br />", "\n").replace("#quot;", "\"").replace("&quot;", "\"").replace("\\n", "\n")
}

fn is_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// An id: letters, digits, `_`, and a `-` between them (`api-server`, but not `A-->B`).
fn read_id(chars: &[char], at: &mut usize) -> Option<String> {
    let start = *at;
    while *at < chars.len() {
        let c = chars[*at];
        if is_id_char(c) || (c == '-' && *at > start && chars.get(*at + 1).is_some_and(|n| is_id_char(*n))) {
            *at += 1;
        } else {
            break;
        }
    }
    (*at > start).then(|| chars[start..*at].iter().collect())
}

fn skip_space(chars: &[char], at: &mut usize) {
    while *at < chars.len() && chars[*at].is_whitespace() {
        *at += 1;
    }
}

fn starts(chars: &[char], at: usize, s: &str) -> bool {
    let s: Vec<char> = s.chars().collect();
    chars.len() >= at + s.len() && chars[at..at + s.len()] == s[..]
}

/// Reads up to the closing `close`, returning what's inside.
fn until(chars: &[char], at: &mut usize, close: &str) -> Option<String> {
    let start = *at;
    while *at < chars.len() {
        if starts(chars, *at, close) {
            let text: String = chars[start..*at].iter().collect();
            *at += close.chars().count();
            return Some(text);
        }
        *at += 1;
    }
    None
}

/// `id`, `id[label]`, `id(label)`, `id{label}`, …, adding it (or its label) to the flow.
fn read_node(chars: &[char], at: &mut usize, flow: &mut Flow) -> Option<usize> {
    let id = read_id(chars, at)?;
    const SHAPES: [(&str, &str, Shape); 12] = [
        ("([", "])", Shape::Stadium),
        ("[(", ")]", Shape::Database),
        ("((", "))", Shape::Circle),
        ("[[", "]]", Shape::Box),
        ("[/", "/]", Shape::Box),
        ("[\\", "\\]", Shape::Box),
        ("{{", "}}", Shape::Diamond),
        ("[", "]", Shape::Box),
        ("(", ")", Shape::Round),
        ("{", "}", Shape::Diamond),
        (">", "]", Shape::Box),
        ("@{", "}", Shape::Box),
    ];
    let mut shape_label: Option<(Shape, String)> = None;
    for (open, close, shape) in SHAPES {
        if starts(chars, *at, open) {
            let mut i = *at + open.chars().count();
            if let Some(label) = until(chars, &mut i, close) {
                *at = i;
                shape_label = Some((shape, clean_label(&label)));
            }
            break;
        }
    }
    let index = match flow.nodes.iter().position(|n| n.id == id) {
        Some(i) => {
            if let Some((shape, label)) = shape_label {
                flow.nodes[i].shape = shape;
                flow.nodes[i].label = label;
            }
            i
        }
        None => {
            let (shape, label) = shape_label.unwrap_or((Shape::Box, id.clone()));
            flow.nodes.push(Node { id, label, shape });
            flow.nodes.len() - 1
        }
    };
    Some(index)
}

/// `A & B`: one or more nodes.
fn read_nodes(chars: &[char], at: &mut usize, flow: &mut Flow) -> Vec<usize> {
    let mut out = Vec::new();
    loop {
        skip_space(chars, at);
        let Some(n) = read_node(chars, at, flow) else { break };
        out.push(n);
        skip_space(chars, at);
        if *at < chars.len() && chars[*at] == '&' {
            *at += 1;
            continue;
        }
        break;
    }
    out
}

/// An edge operator (with its label, if any). None when there's no edge here.
fn read_edge(chars: &[char], at: &mut usize) -> Option<(bool, bool, bool, Option<String>)> {
    skip_space(chars, at);
    let op_char = |c: char| matches!(c, '-' | '=' | '.' | '<' | '>');
    let start = *at;
    while *at < chars.len() && op_char(chars[*at]) {
        *at += 1;
    }
    let mut op: String = chars[start..*at].iter().collect();
    if op.len() < 2 {
        *at = start;
        return None;
    }
    let mut label = None;
    // `-- text -->`, `== text ==>`, `-. text .->`: the label sits inside the arrow.
    if matches!(op.as_str(), "--" | "==" | "-.") && chars.get(*at).is_some_and(|c| c.is_whitespace()) {
        let text_start = *at;
        let mut i = *at;
        let mut found = None;
        while i < chars.len() {
            if op_char(chars[i]) && chars.get(i + 1).is_some_and(|c| op_char(*c)) {
                let j0 = i;
                let mut j = i;
                while j < chars.len() && op_char(chars[j]) {
                    j += 1;
                }
                found = Some((j0, j));
                break;
            }
            i += 1;
        }
        if let Some((j0, j)) = found {
            label = Some(clean_label(&chars[text_start..j0].iter().collect::<String>()));
            op = format!("{op}{}", chars[j0..j].iter().collect::<String>());
            *at = j;
        }
    }
    // `--o` / `--x` ends (circle and cross heads): drawn as arrows.
    let mut arrow = op.ends_with('>');
    if !arrow && (op.ends_with('-') || op.ends_with('=')) && chars.get(*at).is_some_and(|c| *c == 'o' || *c == 'x') && chars.get(*at + 1).is_some_and(|c| c.is_whitespace()) {
        *at += 1;
        arrow = true;
    }
    skip_space(chars, at);
    if chars.get(*at) == Some(&'|') {
        let mut i = *at + 1;
        if let Some(text) = until(chars, &mut i, "|") {
            label = Some(clean_label(&text));
            *at = i;
        }
    }
    let dashed = op.contains('.');
    let thick = op.contains('=');
    Some((dashed, thick, arrow, label.filter(|l| !l.is_empty())))
}

fn parse_chain(statement: &str, flow: &mut Flow) {
    let chars: Vec<char> = statement.chars().collect();
    let mut at = 0;
    let mut left = read_nodes(&chars, &mut at, flow);
    if left.is_empty() {
        return;
    }
    while let Some((dashed, thick, arrow, label)) = read_edge(&chars, &mut at) {
        let right = read_nodes(&chars, &mut at, flow);
        if right.is_empty() {
            break;
        }
        for &from in &left {
            for &to in &right {
                flow.edges.push(Edge { from, to, label: label.clone(), dashed, thick, arrow });
            }
        }
        left = right;
    }
}

// ---- Sequence diagrams ----------------------------------------------------------

fn parse_sequence(lines: &[String]) -> Option<Sequence> {
    let mut seq = Sequence { actors: Vec::new(), steps: Vec::new() };
    fn actor(seq: &mut Sequence, id: &str) -> usize {
        let id = id.trim();
        match seq.actors.iter().position(|(a, _)| a == id) {
            Some(i) => i,
            None => {
                seq.actors.push((id.to_string(), id.to_string()));
                seq.actors.len() - 1
            }
        }
    }
    const ARROWS: [(&str, bool, bool); 8] = [
        ("-->>", true, true),
        ("->>", false, true),
        ("--x", true, true),
        ("-x", false, true),
        ("--)", true, true),
        ("-)", false, true),
        ("-->", true, false),
        ("->", false, false),
    ];
    for line in lines {
        let lower = line.to_ascii_lowercase();
        let word = lower.split_whitespace().next().unwrap_or_default();
        let rest = line[word.len().min(line.len())..].trim();
        match word {
            "participant" | "actor" => {
                let (id, label) = match rest.split_once(" as ") {
                    Some((id, label)) => (id.trim(), clean_label(label)),
                    None => (rest, clean_label(rest)),
                };
                let i = actor(&mut seq, id);
                seq.actors[i].1 = label;
            }
            "note" => {
                let Some((place, text)) = rest.split_once(':') else { continue };
                let place = place.trim();
                let names = place.split_once(' ').map(|(_, n)| n).unwrap_or(place);
                let names = names.trim_start_matches("of ").trim();
                let over: Vec<usize> = names.split(',').map(|n| actor(&mut seq, n)).collect();
                seq.steps.push(Step::Note { over, text: clean_label(text) });
            }
            "loop" | "alt" | "opt" | "par" | "critical" | "break" | "rect" => seq.steps.push(Step::Open(word.to_string(), clean_label(rest))),
            "else" | "and" | "option" => seq.steps.push(Step::Divide(clean_label(rest))),
            "end" => seq.steps.push(Step::Close),
            "autonumber" | "activate" | "deactivate" | "title" | "box" | "create" | "destroy" | "links" | "link" | "properties" => {}
            _ => {
                let Some((head, text)) = line.split_once(':') else { continue };
                let Some((op, dashed, arrow, pos)) = ARROWS.iter().find_map(|(op, d, a)| head.find(op).map(|p| (*op, *d, *a, p))) else { continue };
                let from = head[..pos].trim();
                let to = head[pos + op.len()..].trim().trim_start_matches(['+', '-']).trim();
                if from.is_empty() || to.is_empty() {
                    continue;
                }
                let (from, to) = (actor(&mut seq, from), actor(&mut seq, to));
                seq.steps.push(Step::Message { from, to, text: clean_label(text), dashed, arrow });
            }
        }
    }
    Some(seq)
}

// ---- Layout ---------------------------------------------------------------------

const FONT: f32 = 12.5;
const CHAR_W: f32 = 7.0;
const LINE_H: f32 = 17.;

fn text_width(text: &str) -> f32 {
    text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * CHAR_W
}

/// Wraps to `max` characters a line (labels can be long).
fn wrap(text: &str, max: usize) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        for word in line.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out.join("\n")
}

#[derive(Clone, Debug)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
}

#[derive(Debug)]
pub struct FlowLayout {
    pub width: f32,
    pub height: f32,
    pub nodes: Vec<Placed>,
    /// Ranks per node (for tests and back edges).
    pub rank: Vec<usize>,
}

/// Layers by longest path (back edges reversed), then fewer crossings by
/// ordering each layer around the average position of its neighbours.
pub fn layout_flow(flow: &Flow) -> FlowLayout {
    let n = flow.nodes.len();
    // Back edges, found by a depth-first walk, don't count for layering.
    let mut back = vec![false; flow.edges.len()];
    let mut state = vec![0u8; n];
    let out: Vec<Vec<(usize, usize)>> = (0..n).map(|v| flow.edges.iter().enumerate().filter(|(_, e)| e.from == v && e.to != v).map(|(i, e)| (i, e.to)).collect()).collect();
    fn walk(v: usize, out: &[Vec<(usize, usize)>], state: &mut [u8], back: &mut [bool]) {
        state[v] = 1;
        for &(i, w) in &out[v] {
            match state[w] {
                0 => walk(w, out, state, back),
                1 => back[i] = true,
                _ => {}
            }
        }
        state[v] = 2;
    }
    for v in 0..n {
        if state[v] == 0 {
            walk(v, &out, &mut state, &mut back);
        }
    }
    let forward: Vec<(usize, usize)> = flow.edges.iter().enumerate().filter(|(i, e)| !back[*i] && e.from != e.to).map(|(_, e)| (e.from, e.to)).collect();
    let mut rank = vec![0usize; n];
    // Longest path: relax n times at most (a DAG settles sooner).
    for _ in 0..n {
        let mut changed = false;
        for &(a, b) in &forward {
            if rank[b] < rank[a] + 1 {
                rank[b] = rank[a] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let ranks = rank.iter().copied().max().map_or(0, |m| m + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    for v in 0..n {
        layers[rank[v]].push(v);
    }
    // Order within layers: a few sweeps of barycentres.
    let mut pos = vec![0f32; n];
    let set_pos = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f32>| {
        for layer in layers {
            for (i, &v) in layer.iter().enumerate() {
                pos[v] = i as f32;
            }
        }
    };
    set_pos(&layers, &mut pos);
    for sweep in 0..4 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down { (1..ranks).collect() } else { (0..ranks.saturating_sub(1)).rev().collect() };
        for r in order {
            let mut keyed: Vec<(f32, usize)> = layers[r]
                .iter()
                .map(|&v| {
                    let near: Vec<f32> = forward.iter().filter_map(|&(a, b)| if down && b == v { Some(pos[a]) } else if !down && a == v { Some(pos[b]) } else { None }).collect();
                    let key = if near.is_empty() { pos[v] } else { near.iter().sum::<f32>() / near.len() as f32 };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            layers[r] = keyed.into_iter().map(|(_, v)| v).collect();
            set_pos(&layers, &mut pos);
        }
    }

    // Sizes.
    let horizontal = matches!(flow.dir, Dir::Right | Dir::Left);
    let mut sizes: Vec<(f32, f32, String)> = flow
        .nodes
        .iter()
        .map(|node| {
            let label = wrap(&node.label, 28);
            let lines = label.lines().count().max(1) as f32;
            let mut w = (text_width(&label) + 28.).clamp(72., 230.);
            let mut h = 18. + lines * LINE_H;
            match node.shape {
                Shape::Diamond => {
                    w += 36.;
                    h += 22.;
                }
                Shape::Circle => {
                    let d = w.max(h);
                    w = d;
                    h = d;
                }
                _ => {}
            }
            (w, h, label)
        })
        .collect();
    let has_labels = flow.edges.iter().any(|e| e.label.is_some());
    let (gap_main, gap_cross) = (if has_labels { 64. } else { 48. }, 28.);

    // Each layer's thickness along the flow, and its length across it.
    let thick: Vec<f32> = layers.iter().map(|l| l.iter().map(|&v| if horizontal { sizes[v].0 } else { sizes[v].1 }).fold(0., f32::max)).collect();
    let span: Vec<f32> = layers.iter().map(|l| l.iter().map(|&v| if horizontal { sizes[v].1 } else { sizes[v].0 }).sum::<f32>() + gap_cross * l.len().saturating_sub(1) as f32).collect();
    let widest = span.iter().copied().fold(0., f32::max);
    let pad = 16.;
    let mut placed: Vec<Placed> = vec![Placed { x: 0., y: 0., w: 0., h: 0., label: String::new() }; n];
    let mut main = pad;
    for (r, layer) in layers.iter().enumerate() {
        let mut cross = pad + (widest - span[r]) / 2.;
        for &v in layer {
            let (w, h, label) = std::mem::take(&mut sizes[v]);
            let (x, y) = if horizontal { (main + (thick[r] - w) / 2., cross) } else { (cross, main + (thick[r] - h) / 2.) };
            placed[v] = Placed { x, y, w, h, label };
            cross += if horizontal { h } else { w } + gap_cross;
        }
        main += thick[r] + gap_main;
    }
    let total_main = main - gap_main + pad;
    let total_cross = widest + 2. * pad;
    let (mut width, mut height) = if horizontal { (total_main, total_cross) } else { (total_cross, total_main) };
    // Up and left flow the other way.
    match flow.dir {
        Dir::Up => placed.iter_mut().for_each(|p| p.y = height - p.y - p.h),
        Dir::Left => placed.iter_mut().for_each(|p| p.x = width - p.x - p.w),
        _ => {}
    }
    // Room for back edges, drawn around the side.
    if flow.edges.iter().enumerate().any(|(i, e)| back[i] || e.from == e.to) {
        if horizontal {
            height += 28.;
        } else {
            width += 28.;
        }
    }
    // Room for group titles.
    if !flow.groups.is_empty() {
        width += 16.;
        height += 16.;
        placed.iter_mut().for_each(|p| {
            p.x += 8.;
            p.y += 14.;
        });
    }
    FlowLayout { width, height, nodes: placed, rank }
}

// ---- Drawing --------------------------------------------------------------------

/// An edge to draw: its points, curved, dashed, thick, with an arrow.
type EdgePath = (Vec<(f32, f32)>, bool, bool, bool, bool);
/// A line in a sequence diagram: its points, dashed, width, colour, with an arrow.
type Segment = (Vec<(f32, f32)>, bool, f32, Hsla, bool);

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn arrow_head(window: &mut gpui::Window, tip: Point<Pixels>, from: Point<Pixels>, color: Hsla) {
    let (dx, dy) = (f32::from(tip.x - from.x), f32::from(tip.y - from.y));
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let (size, half) = (8., 4.);
    let base = point(tip.x - px(ux * size), tip.y - px(uy * size));
    let left = point(base.x - px(uy * half), base.y + px(ux * half));
    let right = point(base.x + px(uy * half), base.y - px(ux * half));
    let mut path = PathBuilder::fill();
    path.move_to(tip);
    path.line_to(left);
    path.line_to(right);
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn stroke(window: &mut gpui::Window, points: &[Point<Pixels>], curve: bool, width: f32, dashed: bool, color: Hsla) {
    if points.len() < 2 {
        return;
    }
    let mut path = PathBuilder::stroke(px(width));
    if dashed {
        path = path.dash_array(&[px(4.), px(3.)]);
    }
    path.move_to(points[0]);
    if curve && points.len() == 4 {
        path.cubic_bezier_to(points[3], points[1], points[2]);
    } else {
        for p in &points[1..] {
            path.line_to(*p);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn frame(id: u64, width: f32, height: f32, inner: gpui::Div) -> AnyElement {
    div()
        .id(gpui::ElementId::Name(format!("diagram-{id}").into()))
        .w_full()
        .overflow_x_scroll()
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(theme::line()))
        .bg(rgb(theme::panel()))
        .p(px(12.))
        .child(div().relative().flex_none().w(px(width)).h(px(height)).child(inner))
        .into_any_element()
}

fn label_box(x: f32, y: f32, w: f32, text: String, size: f32) -> gpui::Div {
    div()
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .flex()
        .justify_center()
        .child(
            div()
                .px(px(4.))
                .rounded(px(3.))
                .bg(rgb(theme::panel()))
                .text_size(px(size))
                .line_height(px(size + 4.))
                .text_color(rgb(theme::text_2()))
                .text_center()
                .child(text),
        )
}

/// Draws a ```mermaid block, or None when it isn't one Monaged draws.
pub fn render(code: &str) -> Option<AnyElement> {
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        code.hash(&mut h);
        h.finish()
    };
    match parse(code)? {
        Diagram::Flow(flow) => Some(render_flow(id, &flow)),
        Diagram::Sequence(seq) => Some(render_sequence(id, &seq)),
    }
}

fn render_flow(id: u64, flow: &Flow) -> AnyElement {
    let layout = layout_flow(flow);
    let horizontal = matches!(flow.dir, Dir::Right | Dir::Left);
    let (width, height) = (layout.width, layout.height);
    let nodes = layout.nodes.clone();
    let edges = flow.edges.clone();
    let rank = layout.rank.clone();
    let shapes: Vec<Shape> = flow.nodes.iter().map(|n| n.shape).collect();

    // Group boxes, behind everything.
    let groups: Vec<(f32, f32, f32, f32, String)> = flow
        .groups
        .iter()
        .filter_map(|g| {
            let members: Vec<&Placed> = g.members.iter().filter_map(|&m| nodes.get(m)).collect();
            let x0 = members.iter().map(|p| p.x).fold(f32::MAX, f32::min) - 10.;
            let y0 = members.iter().map(|p| p.y).fold(f32::MAX, f32::min) - 22.;
            let x1 = members.iter().map(|p| p.x + p.w).fold(f32::MIN, f32::max) + 10.;
            let y1 = members.iter().map(|p| p.y + p.h).fold(f32::MIN, f32::max) + 10.;
            (!members.is_empty()).then(|| (x0.max(0.), y0.max(0.), x1 - x0.max(0.), y1 - y0.max(0.), g.title.clone()))
        })
        .collect();

    // Where each edge runs, and where its label goes.
    struct Route {
        points: Vec<(f32, f32)>,
        curve: bool,
        dashed: bool,
        thick: bool,
        arrow: bool,
        label: Option<(f32, f32, String)>,
    }
    let routes: Vec<Route> = edges
        .iter()
        .map(|e| {
            let (a, b) = (&nodes[e.from], &nodes[e.to]);
            let (acx, acy, bcx, bcy) = (a.x + a.w / 2., a.y + a.h / 2., b.x + b.w / 2., b.y + b.h / 2.);
            let side = if horizontal { height - 14. } else { width - 14. };
            let points: Vec<(f32, f32)>;
            let curve;
            if e.from == e.to {
                // To itself: a small loop on its outer side.
                points = if horizontal {
                    vec![(acx - 10., a.y + a.h), (acx - 10., a.y + a.h + 14.), (acx + 10., a.y + a.h + 14.), (acx + 10., a.y + a.h)]
                } else {
                    vec![(a.x + a.w, acy - 8.), (a.x + a.w + 16., acy - 8.), (a.x + a.w + 16., acy + 8.), (a.x + a.w, acy + 8.)]
                };
                curve = false;
            } else if rank[e.to] < rank[e.from] {
                // Back to an earlier layer: around the side.
                points = if horizontal {
                    vec![(acx, a.y + a.h), (acx, side), (bcx, side), (bcx, b.y + b.h)]
                } else {
                    vec![(a.x + a.w, acy), (side, acy), (side, bcy), (b.x + b.w, bcy)]
                };
                curve = false;
            } else {
                let (s, t) = match flow.dir {
                    Dir::Down => ((acx, a.y + a.h), (bcx, b.y)),
                    Dir::Up => ((acx, a.y), (bcx, b.y + b.h)),
                    Dir::Right => ((a.x + a.w, acy), (b.x, bcy)),
                    Dir::Left => ((a.x, acy), (b.x + b.w, bcy)),
                };
                // Same layer: straight across.
                if rank[e.to] == rank[e.from] {
                    let (s, t) = if horizontal {
                        if acy < bcy { ((acx, a.y + a.h), (bcx, b.y)) } else { ((acx, a.y), (bcx, b.y + b.h)) }
                    } else if acx < bcx {
                        ((a.x + a.w, acy), (b.x, bcy))
                    } else {
                        ((a.x, acy), (b.x + b.w, bcy))
                    };
                    points = vec![s, t];
                    curve = false;
                } else {
                    let (c1, c2) = if horizontal { ((s.0 + (t.0 - s.0) / 2., s.1), (s.0 + (t.0 - s.0) / 2., t.1)) } else { ((s.0, s.1 + (t.1 - s.1) / 2.), (t.0, s.1 + (t.1 - s.1) / 2.)) };
                    points = vec![s, c1, c2, t];
                    curve = true;
                }
            }
            let label = e.label.clone().map(|l| {
                let (mx, my) = if points.len() == 4 {
                    let p = |i: usize| points[i];
                    (0.125 * p(0).0 + 0.375 * p(1).0 + 0.375 * p(2).0 + 0.125 * p(3).0, 0.125 * p(0).1 + 0.375 * p(1).1 + 0.375 * p(2).1 + 0.125 * p(3).1)
                } else {
                    ((points[0].0 + points[1].0) / 2., (points[0].1 + points[1].1) / 2.)
                };
                let text = wrap(&l, 24);
                let w = text_width(&text) + 10.;
                (mx - w / 2., my - 9., text)
            });
            Route { points, curve, dashed: e.dashed, thick: e.thick, arrow: e.arrow, label }
        })
        .collect();

    let edge_color = color(theme::text_3());
    let node_fill = color(theme::raised());
    let node_line = color(theme::line_strong());
    let paths: Vec<EdgePath> = routes.iter().map(|r| (r.points.clone(), r.curve, r.dashed, r.thick, r.arrow)).collect();
    let diamonds: Vec<Placed> = nodes.iter().zip(&shapes).filter(|(_, s)| **s == Shape::Diamond).map(|(p, _)| p.clone()).collect();
    let group_line = color(theme::line());
    let group_boxes = groups.clone();
    let drawing = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            for (x, y, w, h, _) in &group_boxes {
                stroke(window, &[at(*x, *y), at(x + w, *y), at(x + w, y + h), at(*x, y + h), at(*x, *y)], false, 1., true, group_line);
            }
            for (points, curve, dashed, thick, arrow) in &paths {
                let pts: Vec<Point<Pixels>> = points.iter().map(|(x, y)| at(*x, *y)).collect();
                stroke(window, &pts, *curve, if *thick { 2.2 } else { 1.3 }, *dashed, edge_color);
                if *arrow && pts.len() >= 2 {
                    let tip = pts[pts.len() - 1];
                    let before = pts[pts.len() - 2];
                    arrow_head(window, tip, before, edge_color);
                }
            }
            for d in &diamonds {
                let (cx, cy) = (d.x + d.w / 2., d.y + d.h / 2.);
                let corners = [at(cx, d.y), at(d.x + d.w, cy), at(cx, d.y + d.h), at(d.x, cy)];
                let mut fill = PathBuilder::fill();
                fill.move_to(corners[0]);
                for c in &corners[1..] {
                    fill.line_to(*c);
                }
                fill.close();
                if let Ok(path) = fill.build() {
                    window.paint_path(path, node_fill);
                }
                stroke(window, &[corners[0], corners[1], corners[2], corners[3], corners[0]], false, 1., false, node_line);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(width))
    .h(px(height));

    let mut inner = div().absolute().top_0().left_0().w(px(width)).h(px(height));
    for (x, y, _, _, title) in &groups {
        inner = inner.child(div().absolute().left(px(x + 6.)).top(px(y + 3.)).text_size(px(11.)).text_color(rgb(theme::text_3())).child(title.clone()));
    }
    inner = inner.child(drawing);
    for (p, shape) in nodes.iter().zip(&shapes) {
        let radius = match shape {
            Shape::Box => 4.,
            Shape::Round => 10.,
            Shape::Database => 8.,
            Shape::Stadium | Shape::Circle => p.h / 2.,
            Shape::Diamond => 0.,
        };
        let mut node = div()
            .absolute()
            .left(px(p.x))
            .top(px(p.y))
            .w(px(p.w))
            .h(px(p.h))
            .flex()
            .items_center()
            .justify_center()
            .px(px(10.))
            .text_size(px(FONT))
            .line_height(px(LINE_H))
            .text_color(rgb(theme::text()))
            .text_center();
        if *shape != Shape::Diamond {
            node = node.rounded(px(radius)).border_1().border_color(rgb(theme::line_strong())).bg(rgb(theme::raised()));
        }
        if *shape == Shape::Database {
            node = node.border_t_2();
        }
        inner = inner.child(node.child(p.label.clone()));
    }
    for route in &routes {
        if let Some((x, y, text)) = &route.label {
            inner = inner.child(label_box(*x, *y, text_width(text) + 10., text.clone(), 11.));
        }
    }
    frame(id, width, height, inner)
}

fn render_sequence(id: u64, seq: &Sequence) -> AnyElement {
    let pad = 12.;
    let head_h = 32.;
    let widths: Vec<f32> = seq.actors.iter().map(|(_, l)| (text_width(&wrap(l, 22)) + 28.).clamp(90., 200.)).collect();
    // Columns wide enough for the longest message between neighbours.
    let mut gaps = vec![60f32; seq.actors.len()];
    for step in &seq.steps {
        if let Step::Message { from, to, text, .. } = step
            && from != to
        {
            let (a, b) = ((*from).min(*to), (*from).max(*to));
            let need = (text_width(&wrap(text, 40)) + 30.) / (b - a) as f32;
            for g in gaps.iter_mut().take(b).skip(a) {
                *g = g.max(need - 60.);
            }
        }
    }
    let mut centers = Vec::new();
    let mut x = pad;
    for (i, w) in widths.iter().enumerate() {
        let col = w.max(120.);
        centers.push(x + col / 2.);
        x += col + gaps[i].min(260.);
    }
    let width = x - gaps.last().copied().unwrap_or(0.).min(260.) + pad + 40.;

    // Rows.
    struct Row {
        y: f32,
        step: Step,
    }
    let mut rows = Vec::new();
    let mut y = pad + head_h + 18.;
    for step in &seq.steps {
        let h = match step {
            Step::Message { from, to, text, .. } => {
                let lines = wrap(text, 40).lines().count().max(1) as f32;
                if from == to { 34. + lines * 14. } else { 22. + lines * 14. }
            }
            Step::Note { text, .. } => 16. + wrap(text, 34).lines().count().max(1) as f32 * 16.,
            Step::Open(..) | Step::Divide(_) => 26.,
            Step::Close => 10.,
        };
        rows.push(Row { y, step: step.clone() });
        y += h + 6.;
    }
    let height = y + pad;
    let left = pad - 4.;
    let right = width - pad;

    let line = color(theme::line_strong());
    let edge = color(theme::text_3());
    let frame_line = color(theme::line());
    let mut segments: Vec<Segment> = Vec::new();
    for &cx in &centers {
        segments.push((vec![(cx, pad + head_h), (cx, height - pad)], true, 1., line, false));
    }
    let mut labels: Vec<(f32, f32, f32, String, bool)> = Vec::new();
    let mut open_frames: Vec<f32> = Vec::new();
    for row in &rows {
        match &row.step {
            Step::Message { from, to, text, dashed, arrow } => {
                let text = wrap(text, 40);
                let lines = text.lines().count().max(1) as f32;
                let label_h = lines * 14.;
                let (a, b) = (centers[*from], centers[*to]);
                let my = row.y + label_h + 6.;
                if from == to {
                    segments.push((vec![(a, my), (a + 34., my), (a + 34., my + 16.), (a + 2., my + 16.)], *dashed, 1.3, edge, *arrow));
                    labels.push((a + 6., row.y, text_width(&text) + 10., text, false));
                } else {
                    segments.push((vec![(a, my), (if b > a { b - 1. } else { b + 1. }, my)], *dashed, 1.3, edge, *arrow));
                    let w = text_width(&text) + 10.;
                    labels.push(((a + b) / 2. - w / 2., row.y, w, text, false));
                }
            }
            Step::Note { over, text } => {
                let xs: Vec<f32> = over.iter().filter_map(|&i| centers.get(i).copied()).collect();
                let text = wrap(text, 34);
                let w = (text_width(&text) + 16.).max(xs.iter().copied().fold(f32::MIN, f32::max) - xs.iter().copied().fold(f32::MAX, f32::min) + 40.);
                let mid = xs.iter().sum::<f32>() / xs.len().max(1) as f32;
                labels.push((mid - w / 2., row.y, w, text, true));
            }
            Step::Open(kind, label) => {
                open_frames.push(row.y);
                labels.push((left + 4., row.y + 4., text_width(&format!("{kind} {label}")) + 12., format!("{kind} {label}"), false));
            }
            Step::Divide(label) => {
                segments.push((vec![(left, row.y + 2.), (right, row.y + 2.)], true, 1., frame_line, false));
                if !label.is_empty() {
                    labels.push((left + 4., row.y + 6., text_width(&format!("else {label}")) + 12., format!("else {label}"), false));
                }
            }
            Step::Close => {
                if let Some(top) = open_frames.pop() {
                    segments.push((vec![(left, top), (right, top), (right, row.y), (left, row.y), (left, top)], false, 1., frame_line, false));
                }
            }
        }
    }
    let drawing = canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            for (points, dashed, width, color, arrow) in &segments {
                let pts: Vec<Point<Pixels>> = points.iter().map(|(x, y)| at(*x, *y)).collect();
                stroke(window, &pts, false, *width, *dashed, *color);
                if *arrow {
                    arrow_head(window, pts[pts.len() - 1], pts[pts.len() - 2], *color);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(width))
    .h(px(height));

    let mut inner = div().absolute().top_0().left_0().w(px(width)).h(px(height)).child(drawing);
    for (i, (_, label)) in seq.actors.iter().enumerate() {
        let w = widths[i];
        inner = inner.child(
            div()
                .absolute()
                .left(px(centers[i] - w / 2.))
                .top(px(pad))
                .w(px(w))
                .h(px(head_h))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(theme::line_strong()))
                .bg(rgb(theme::raised()))
                .text_size(px(FONT))
                .text_color(rgb(theme::text()))
                .text_center()
                .child(wrap(label, 22)),
        );
    }
    for (x, y, w, text, note) in labels {
        if note {
            inner = inner.child(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .bg(rgb(theme::selected()))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(rgb(theme::text_2()))
                    .text_center()
                    .child(text),
            );
        } else {
            inner = inner.child(label_box(x, y, w, text, 11.5));
        }
    }
    frame(id, width, height, inner)
}

/// A flowchart of the plan's workstreams and what each waits for (the
/// build order), as Mermaid, for the plan page.
pub fn build_order(streams: &[crate::pipeline::Workstream]) -> Option<String> {
    if streams.is_empty() {
        return None;
    }
    let mut code = String::from("flowchart LR\n");
    let names: HashMap<&str, usize> = streams.iter().enumerate().map(|(i, w)| (w.name.as_str(), i)).collect();
    for (i, w) in streams.iter().enumerate() {
        code.push_str(&format!("  w{i}[\"{}\"]\n", w.name.replace('"', "'")));
    }
    for (i, w) in streams.iter().enumerate() {
        for dep in w.depends_on() {
            if let Some(&d) = names.get(dep.as_str()) {
                code.push_str(&format!("  w{d} --> w{i}\n"));
            }
        }
    }
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(code: &str) -> Flow {
        match parse(code) {
            Some(Diagram::Flow(f)) => f,
            other => panic!("not a flowchart: {other:?}"),
        }
    }

    #[test]
    fn flowcharts_read_shapes_labels_and_chains() {
        let f = flow(
            "flowchart LR\n  ui[Web UI] -->|REST| api(API server)\n  api --> db[(Postgres)] & cache{{Redis}}\n  api -.-> queue([Jobs]) --> worker((Worker))\n  %% a comment\n  classDef x fill:#f00\n",
        );
        assert_eq!(f.dir, Dir::Right);
        let label = |id: &str| f.nodes.iter().find(|n| n.id == id).map(|n| (n.label.clone(), n.shape)).unwrap();
        assert_eq!(label("ui"), ("Web UI".into(), Shape::Box));
        assert_eq!(label("api"), ("API server".into(), Shape::Round));
        assert_eq!(label("db"), ("Postgres".into(), Shape::Database));
        assert_eq!(label("cache"), ("Redis".into(), Shape::Diamond));
        assert_eq!(label("queue"), ("Jobs".into(), Shape::Stadium));
        assert_eq!(label("worker"), ("Worker".into(), Shape::Circle));
        assert_eq!(f.edges.len(), 5);
        assert_eq!(f.edges[0].label.as_deref(), Some("REST"));
        let dotted = f.edges.iter().find(|e| f.nodes[e.to].id == "queue").unwrap();
        assert!(dotted.dashed && dotted.arrow);
    }

    #[test]
    fn labels_inside_arrows_dashes_in_ids_and_quotes() {
        let f = flow("graph TD\n  user-service[\"User service<br/>(Rust)\"] -- calls --> auth-db\n  a == sync ==> b\n  c --- d");
        assert_eq!(f.nodes[0].id, "user-service");
        assert_eq!(f.nodes[0].label, "User service\n(Rust)");
        assert_eq!(f.edges[0].label.as_deref(), Some("calls"));
        assert!(f.edges[1].thick && f.edges[1].label.as_deref() == Some("sync"));
        assert!(!f.edges[2].arrow);
    }

    #[test]
    fn subgraphs_group_their_nodes() {
        let f = flow("flowchart TD\n subgraph Backend [The backend]\n  api --> db\n end\n web --> api");
        assert_eq!(f.groups.len(), 1);
        assert_eq!(f.groups[0].title, "The backend");
        let ids: Vec<&str> = f.groups[0].members.iter().map(|&m| f.nodes[m].id.as_str()).collect();
        assert_eq!(ids, ["api", "db"]);
    }

    #[test]
    fn layers_follow_the_edges_and_survive_cycles() {
        let f = flow("flowchart TD\n a --> b --> c\n a --> c\n c --> a");
        let l = layout_flow(&f);
        let rank = |id: &str| l.rank[f.nodes.iter().position(|n| n.id == id).unwrap()];
        assert_eq!((rank("a"), rank("b"), rank("c")), (0, 1, 2));
        // Top to bottom: each layer lower than the one before.
        let y = |id: &str| l.nodes[f.nodes.iter().position(|n| n.id == id).unwrap()].y;
        assert!(y("a") < y("b") && y("b") < y("c"));
        assert!(l.width > 0. && l.height > 0.);
    }

    #[test]
    fn left_to_right_runs_along_x() {
        let f = flow("flowchart LR\n a --> b");
        let l = layout_flow(&f);
        assert!(l.nodes[0].x < l.nodes[1].x);
        assert!((l.nodes[0].y - l.nodes[1].y).abs() < 1.);
    }

    #[test]
    fn sequence_diagrams_read_actors_messages_notes_and_frames() {
        let Some(Diagram::Sequence(s)) = parse(
            "sequenceDiagram\n  actor U as User\n  participant API\n  U->>API: POST /reservations\n  API-->>U: 201 Created\n  Note over U,API: authenticated\n  loop every minute\n    API->>API: expire holds\n  end\n  alt conflict\n    API-->>U: 409\n  else ok\n    API-->>U: 200\n  end",
        ) else {
            panic!("not a sequence diagram")
        };
        assert_eq!(s.actors, vec![("U".into(), "User".into()), ("API".into(), "API".into())]);
        assert!(matches!(&s.steps[0], Step::Message { from: 0, to: 1, text, dashed: false, .. } if text == "POST /reservations"));
        assert!(matches!(&s.steps[1], Step::Message { from: 1, to: 0, dashed: true, .. }));
        assert!(matches!(&s.steps[2], Step::Note { over, .. } if over == &vec![0, 1]));
        assert!(matches!(&s.steps[3], Step::Open(kind, label) if kind == "loop" && label == "every minute"));
        assert!(s.steps.iter().any(|st| matches!(st, Step::Divide(l) if l == "ok")));
    }

    #[test]
    fn other_diagrams_stay_code() {
        assert!(parse("pie title Pets\n \"Dogs\" : 386").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn the_build_order_comes_from_the_workstreams() {
        let plan = serde_json::json!({ "workstreams": [
            { "name": "core", "depends_on": [] },
            { "name": "api", "depends_on": ["core"] },
            { "name": "ui", "depends_on": ["api"] }
        ]});
        let code = build_order(&crate::pipeline::workstreams(&plan)).unwrap();
        let f = flow(&code);
        assert_eq!(f.nodes.len(), 3);
        assert_eq!(f.edges.len(), 2);
        let l = layout_flow(&f);
        assert!(l.nodes[0].x < l.nodes[1].x && l.nodes[1].x < l.nodes[2].x);
    }
}
