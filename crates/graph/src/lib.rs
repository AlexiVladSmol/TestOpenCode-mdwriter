//! Граф связей заметок: построение из индекса + force-направленный layout.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::Vec2;
use index::Index;
use petgraph::graph::{Graph as PGraph, NodeIndex};
use petgraph::Undirected;

/// Тип неориентированного графа заметок.
type GraphTy = PGraph<String, (), Undirected>;

/// Граф заметок с позициями узлов для отрисовки.
pub struct GraphView {
    graph: GraphTy,
    /// Параллельные массивы (индексы узлов без удалений идут 0..n).
    paths: Vec<PathBuf>,
    titles: Vec<String>,
    pos: Vec<Vec2>,
    vel: Vec<Vec2>,
}

impl Default for GraphView {
    fn default() -> Self {
        Self {
            graph: GraphTy::new_undirected(),
            paths: Vec::new(),
            titles: Vec::new(),
            pos: Vec::new(),
            vel: Vec::new(),
        }
    }
}

impl GraphView {
    /// Пересобирает граф из индекса, сохраняя позиции уже известных заметок.
    pub fn update(&mut self, index: &Index) {
        let old: HashMap<PathBuf, Vec2> = self
            .paths
            .iter()
            .zip(self.pos.iter())
            .map(|(p, v)| (p.clone(), *v))
            .collect();

        self.rebuild(index);

        for (i, p) in self.paths.iter().enumerate() {
            if let Some(v) = old.get(p) {
                self.pos[i] = *v;
                self.vel[i] = Vec2::ZERO;
            }
        }
    }

    fn rebuild(&mut self, index: &Index) {
        let entries = index.switcher_entries();

        let mut graph = GraphTy::new_undirected();
        let mut node_ids: HashMap<PathBuf, NodeIndex> = HashMap::new();
        let mut paths = Vec::with_capacity(entries.len());
        let mut titles = Vec::with_capacity(entries.len());

        for (path, title) in entries {
            let id = graph.add_node(title.clone());
            node_ids.insert(path.clone(), id);
            paths.push(path);
            titles.push(title);
        }

        for (path, id) in node_ids.iter() {
            if let Some(meta) = index.meta(path) {
                for target in &meta.links_out {
                    if let Some(to) = index.resolve_wikilink(path, target) {
                        if let Some(to_id) = node_ids.get(&to) {
                            if to_id != id {
                                graph.update_edge(*id, *to_id, ());
                            }
                        }
                    }
                }
            }
        }

        let n = graph.node_count();
        let mut pos = Vec::with_capacity(n);
        let vel = vec![Vec2::ZERO; n];
        let radius = (n as f32 * 18.0).max(140.0);
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            pos.push(Vec2::new(a.cos(), a.sin()) * radius);
        }

        self.graph = graph;
        self.paths = paths;
        self.titles = titles;
        self.pos = pos;
        self.vel = vel;
    }

    /// Один шаг симуляции (отталкивание + пружины + гравитация к центру).
    pub fn step(&mut self) {
        let n = self.pos.len();
        if n < 2 {
            return;
        }
        let k = 110.0; // «идеальная» длина ребра

        let mut forces = vec![Vec2::ZERO; n];

        // Отталкивание (Кулон), O(n²) — для небольших vault'ов нормально.
        for i in 0..n {
            for j in (i + 1)..n {
                let d = self.pos[j] - self.pos[i];
                let len = d.length().max(1.0);
                let f = k * k / len;
                let dir = d / len;
                forces[i] -= dir * f;
                forces[j] += dir * f;
            }
        }

        // Пружины вдоль рёбер.
        for e in self.graph.edge_indices() {
            if let Some((a, b)) = self.graph.edge_endpoints(e) {
                let d = self.pos[b.index()] - self.pos[a.index()];
                let len = d.length().max(0.001);
                let f = (len - k) * 0.04;
                let dir = d / len;
                forces[a.index()] += dir * f;
                forces[b.index()] -= dir * f;
            }
        }

        // Гравитация к центру.
        for (f, p) in forces.iter_mut().zip(self.pos.iter()) {
            *f -= *p * 0.03;
        }

        // Интегрирование с демпфированием.
        for (p, (v, f)) in self
            .pos
            .iter_mut()
            .zip(self.vel.iter_mut().zip(forces.iter()))
        {
            *v = (*v + *f * 0.08) * 0.85;
            if v.length() < 0.001 {
                *v = Vec2::ZERO;
            }
            *p += *v;
        }
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.node_count() == 0
    }

    /// (заголовок, путь, позиция) узла по индексу.
    pub fn node(&self, i: usize) -> Option<(&str, &PathBuf, egui::Pos2)> {
        self.titles
            .get(i)
            .map(|t| (t.as_str(), &self.paths[i], self.pos[i].to_pos2()))
    }

    /// Пары индексов узлов для всех рёбер.
    pub fn edge_pairs(&self) -> Vec<(usize, usize)> {
        self.graph
            .edge_indices()
            .filter_map(|e| {
                self.graph
                    .edge_endpoints(e)
                    .map(|(a, b)| (a.index(), b.index()))
            })
            .collect()
    }
}
