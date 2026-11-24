//! Граф связей заметок: построение из индекса + force-направленный layout.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::Vec2;
use index::Index;
use petgraph::graph::{Graph as PGraph, NodeIndex};
use petgraph::Undirected;

/// Тип неориентированного графа заметок.
type GraphTy = PGraph<String, (), Undirected>;

/// Порог «граф успокоился»: **средняя** кинетическая энергия на узел.
///
/// Сравнивать суммарную энергию нельзя: она растёт с числом узлов, и граф из
/// 300+ заметок не затихал бы никогда — даже за 3000 кадров (замер Claude).
const SETTLE_ENERGY_PER_NODE: f32 = 0.02;

/// Страховка: максимум шагов симуляции на одну раскладку.
///
/// На больших графах энергия не сходится к нулю вообще: силы отталкивания
/// растут с числом узлов, и средняя энергия на узел колеблется в десятках
/// (замер на 320 заметках: от 27 до 440 даже после 1200 шагов). Поэтому
/// порог ловит малые и средние графы, а большие останавливает этот бюджет —
/// иначе окно графа крутило бы симуляцию бесконечно.
const MAX_ANIMATION_STEPS: u32 = 900;

/// Граф заметок с позициями узлов для отрисовки.
pub struct GraphView {
    graph: GraphTy,
    /// Параллельные массивы (индексы узлов без удалений идут 0..n).
    paths: Vec<PathBuf>,
    titles: Vec<String>,
    pos: Vec<Vec2>,
    vel: Vec<Vec2>,
    /// Симуляция успокоилась: шаги прекращаются, пока граф не изменят.
    settled: bool,
    /// Сколько шагов сделано с последнего `update`/`wake` (страховка от
    /// бесконечной анимации).
    steps_done: u32,
}

impl Default for GraphView {
    fn default() -> Self {
        Self {
            graph: GraphTy::new_undirected(),
            paths: Vec::new(),
            titles: Vec::new(),
            pos: Vec::new(),
            vel: Vec::new(),
            settled: false,
            steps_done: 0,
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
        // Новый граф — снова есть что раскладывать, бюджет шагов заново.
        self.settled = false;
        self.steps_done = 0;

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

        // Связи уже разрешены индексом (без ссылок на себя и дублей).
        for (path, id) in node_ids.iter() {
            for to in index.links(path) {
                if let Some(to_id) = node_ids.get(to) {
                    graph.update_edge(*id, *to_id, ());
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
    /// Возвращает суммарную кинетическую энергию после шага.
    pub fn step(&mut self) -> f32 {
        let n = self.pos.len();
        if n < 2 {
            return 0.0;
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

        self.vel.iter().map(|v| v.length_sq()).sum()
    }

    /// Делает до `steps` шагов, пока симуляция не успокоится.
    /// Возвращает `true`, если граф ещё раскладывается и нужен новый кадр.
    pub fn animate(&mut self, steps: usize) -> bool {
        if self.settled {
            return false;
        }
        let nodes = self.pos.len().max(1) as f32;
        for _ in 0..steps.max(1) {
            let energy = self.step();
            self.steps_done += 1;
            let settled_by_energy = energy / nodes < SETTLE_ENERGY_PER_NODE;
            let out_of_budget = self.steps_done >= MAX_ANIMATION_STEPS;
            if settled_by_energy || out_of_budget {
                self.settled = true;
                break;
            }
        }
        !self.settled
    }

    /// Успокоилась ли симуляция (шаги не нужны, перерисовывать нечего).
    pub fn settled(&self) -> bool {
        self.settled
    }

    /// Будит симуляцию: граф изменили или пользователь тронул узел.
    pub fn wake(&mut self) {
        self.settled = false;
        self.steps_done = 0;
    }

    /// Сколько шагов симуляции сделано с последней сборки/пробуждения.
    pub fn steps_done(&self) -> u32 {
        self.steps_done
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Пустой временный vault (удаляется вручную в конце теста).
    fn temp_vault(tag: &str) -> (vault::Vault, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mdwriter-graph-{tag}-{unique}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (vault::Vault::open(dir.clone()), dir)
    }

    fn index_with_two_linked_notes(tag: &str) -> (Index, PathBuf) {
        let (vault, dir) = temp_vault(tag);
        vault.write(Path::new("a.md"), "Ссылка на [[b]]").unwrap();
        vault.write(Path::new("b.md"), "Ссылка на [[a]]").unwrap();
        (Index::build(&vault).unwrap(), dir)
    }

    /// C4: симуляция засыпает, когда узлы перестают двигаться, и снова
    /// просыпается при изменении графа.
    #[test]
    fn simulation_settles_and_wakes_on_update() {
        let (index, dir) = index_with_two_linked_notes("settle");
        let mut graph = GraphView::default();
        graph.update(&index);
        assert!(!graph.settled(), "после сборки есть что раскладывать");

        let mut frames = 0;
        while graph.animate(3) {
            frames += 1;
            assert!(frames < 5000, "симуляция не успокоилась за {frames} кадров");
        }
        assert!(graph.settled());
        // Успокоившийся граф кадров не требует.
        assert!(!graph.animate(3));

        // Изменение индекса будит симуляцию.
        graph.update(&index);
        assert!(!graph.settled());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// C4: пустой граф сразу считается успокоившимся.
    #[test]
    fn empty_graph_is_settled_immediately() {
        let (vault, dir) = temp_vault("empty");
        let index = Index::build(&vault).unwrap();
        let mut graph = GraphView::default();
        graph.update(&index);
        assert_eq!(graph.node_count(), 0);
        assert!(!graph.animate(3));
        assert!(graph.settled());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Граф из 300+ заметок (та же плотность связей, что в реальном vault)
    /// должен успокаиваться за ограниченное число кадров, а не крутиться вечно.
    #[test]
    fn large_graph_settles_within_bounded_frames() {
        let (vault, dir) = temp_vault("big");
        let n = 320;
        for i in 0..n {
            let links: String = (1..=3)
                .map(|k| format!("[[note{}]]", (i + k * 7) % n))
                .collect::<Vec<_>>()
                .join(" ");
            vault
                .write(
                    Path::new(&format!("note{i}.md")),
                    &format!("# {i}\n{links}"),
                )
                .unwrap();
        }
        let index = Index::build(&vault).unwrap();
        let mut graph = GraphView::default();
        graph.update(&index);
        assert_eq!(graph.node_count(), n);

        let frames = count_frames_until_settled(&mut graph);
        assert!(
            graph.settled(),
            "не успокоился за {frames} кадров ({} шагов)",
            graph.steps_done()
        );
        assert!(
            frames <= (MAX_ANIMATION_STEPS as usize / 3) + 1,
            "кадров больше бюджета: {frames}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Крутит `animate(3)`, как окно графа, и считает кадры.
    fn count_frames_until_settled(graph: &mut GraphView) -> usize {
        let mut frames = 0;
        while graph.animate(3) {
            frames += 1;
            assert!(frames < 5000, "симуляция не заканчивается");
        }
        frames
    }

    #[test]
    fn wake_restarts_the_simulation() {
        let (index, dir) = index_with_two_linked_notes("wake");
        let mut graph = GraphView::default();
        graph.update(&index);
        while graph.animate(3) {}
        assert!(graph.settled());

        graph.wake();
        assert!(!graph.settled(), "после wake шаги снова разрешены");
        // Позиции не менялись, поэтому энергия нулевая и граф тут же засыпает —
        // wake не «крутит» симуляцию сам по себе.
        assert!(!graph.animate(3));
        assert!(graph.settled());
        let _ = std::fs::remove_dir_all(dir);
    }
}
