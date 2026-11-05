//! Построение и отрисовка дерева файлов vault.

use std::path::{Path, PathBuf};

use eframe::egui;

#[derive(Debug, Clone)]
pub enum TreeNode {
    Dir { name: String, children: Vec<TreeNode> },
    File { name: String, rel: PathBuf },
}

impl TreeNode {
    fn name(&self) -> &str {
        match self {
            TreeNode::Dir { name, .. } | TreeNode::File { name, .. } => name,
        }
    }
}

/// Строит вложенное дерево из списка относительных путей заметок.
pub fn build_tree(notes: &[PathBuf]) -> Vec<TreeNode> {
    let mut roots = Vec::new();
    for note in notes {
        let comps: Vec<String> = note
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        insert(&mut roots, &comps, note);
    }
    sort_tree(&mut roots);
    roots
}

fn insert(nodes: &mut Vec<TreeNode>, comps: &[String], rel: &Path) {
    let Some((first, rest)) = comps.split_first() else {
        return;
    };

    if rest.is_empty() {
        nodes.push(TreeNode::File {
            name: first.clone(),
            rel: rel.to_path_buf(),
        });
        return;
    }

    let existing = nodes.iter_mut().find_map(|n| match n {
        TreeNode::Dir { name, children } if name == first => Some(children),
        _ => None,
    });

    if let Some(children) = existing {
        insert(children, rest, rel);
    } else {
        nodes.push(TreeNode::Dir {
            name: first.clone(),
            children: Vec::new(),
        });
        if let Some(TreeNode::Dir { children, .. }) = nodes.last_mut() {
            insert(children, rest, rel);
        }
    }
}

fn sort_tree(nodes: &mut [TreeNode]) {
    nodes.sort_by(|a, b| a.name().cmp(b.name()));
    for node in nodes {
        if let TreeNode::Dir { children, .. } = node {
            sort_tree(children);
        }
    }
}

/// Рисует дерево, возвращает относительный путь кликнутого файла (если есть).
pub fn show_tree(
    ui: &mut egui::Ui,
    nodes: &[TreeNode],
    selected: &Option<PathBuf>,
) -> Option<PathBuf> {
    let mut clicked = None;
    for node in nodes {
        if let Some(p) = show_node(ui, node, selected) {
            clicked = Some(p);
        }
    }
    clicked
}

fn show_node(
    ui: &mut egui::Ui,
    node: &TreeNode,
    selected: &Option<PathBuf>,
) -> Option<PathBuf> {
    match node {
        TreeNode::Dir { name, children } => {
            let mut clicked = None;
            egui::CollapsingHeader::new(name)
                .default_open(true)
                .show(ui, |ui| {
                    for child in children {
                        if let Some(p) = show_node(ui, child, selected) {
                            clicked = Some(p);
                        }
                    }
                });
            clicked
        }
        TreeNode::File { name, rel } => {
            let is_selected = selected.as_deref() == Some(rel.as_path());
            if ui.selectable_label(is_selected, name).clicked() {
                Some(rel.clone())
            } else {
                None
            }
        }
    }
}
