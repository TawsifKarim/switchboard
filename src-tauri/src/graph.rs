use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config::AppEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    SelfLoop(String),
    UnknownDep { app: String, dep: String },
    Cycle(Vec<String>),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::SelfLoop(id) => write!(f, "app {id} depends on itself"),
            GraphError::UnknownDep { app, dep } => {
                write!(f, "app {app} depends on unknown id {dep}")
            }
            GraphError::Cycle(ids) => {
                write!(f, "dependency cycle: {}", ids.join(" → "))
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// Kahn-style topological layering. Each returned `Vec<String>` is a set of
/// app ids whose dependencies are all satisfied by ids in earlier levels (or
/// have none). The caller starts a level in parallel and waits for every
/// app's readiness signal before advancing.
///
/// Ids within a level are returned in sorted order for deterministic output.
pub fn topo_levels(apps: &[AppEntry]) -> Result<Vec<Vec<String>>, GraphError> {
    let ids: HashSet<&str> = apps.iter().map(|a| a.id.as_str()).collect();
    for a in apps {
        for d in &a.depends_on {
            if d == &a.id {
                return Err(GraphError::SelfLoop(a.id.clone()));
            }
            if !ids.contains(d.as_str()) {
                return Err(GraphError::UnknownDep {
                    app: a.id.clone(),
                    dep: d.clone(),
                });
            }
        }
    }

    let mut indegree: BTreeMap<String, usize> =
        apps.iter().map(|a| (a.id.clone(), 0)).collect();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for a in apps {
        // Dedupe within an app's own list — `depends_on: [x, x]` is one edge.
        let mut deps: Vec<&String> = a.depends_on.iter().collect();
        deps.sort();
        deps.dedup();
        for d in deps {
            *indegree.entry(a.id.clone()).or_insert(0) += 1;
            children.entry(d.clone()).or_default().push(a.id.clone());
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut remaining = apps.len();
    let mut current: Vec<String> = indegree
        .iter()
        .filter(|(_, &n)| n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    current.sort();

    while !current.is_empty() {
        remaining -= current.len();
        let mut next: Vec<String> = Vec::new();
        for parent in &current {
            for child in children.get(parent).cloned().unwrap_or_default() {
                if let Some(n) = indegree.get_mut(&child) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        next.push(child);
                    }
                }
            }
        }
        levels.push(current);
        next.sort();
        next.dedup();
        current = next;
    }

    if remaining > 0 {
        let stuck: Vec<String> = indegree
            .iter()
            .filter(|(_, &n)| n > 0)
            .map(|(id, _)| id.clone())
            .collect();
        return Err(GraphError::Cycle(find_cycle(apps, &stuck)));
    }

    Ok(levels)
}

/// Walk parent edges from any stuck node until we revisit one — that revisit
/// is the cycle. The returned vec lists ids in traversal order with the
/// repeating id appended (so `a → b → c → a` reads naturally).
fn find_cycle(apps: &[AppEntry], stuck: &[String]) -> Vec<String> {
    let parents: HashMap<&str, &[String]> = apps
        .iter()
        .map(|a| (a.id.as_str(), a.depends_on.as_slice()))
        .collect();
    let stuck_set: HashSet<&str> = stuck.iter().map(String::as_str).collect();

    fn dfs(
        node: &str,
        parents: &HashMap<&str, &[String]>,
        stuck: &HashSet<&str>,
        path: &mut Vec<String>,
        on_path: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        if on_path.contains(node) {
            let start = path.iter().position(|x| x == node).unwrap();
            let mut cyc: Vec<String> = path[start..].to_vec();
            cyc.push(node.to_string());
            return Some(cyc);
        }
        if !stuck.contains(node) {
            return None;
        }
        on_path.insert(node.to_string());
        path.push(node.to_string());
        for dep in parents.get(node).copied().unwrap_or(&[]) {
            if let Some(c) = dfs(dep, parents, stuck, path, on_path) {
                return Some(c);
            }
        }
        path.pop();
        on_path.remove(node);
        None
    }

    let mut sorted_stuck: Vec<&String> = stuck.iter().collect();
    sorted_stuck.sort();
    for s in sorted_stuck {
        let mut path = Vec::new();
        let mut on_path = HashSet::new();
        if let Some(c) = dfs(s, &parents, &stuck_set, &mut path, &mut on_path) {
            return c;
        }
    }
    stuck.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppEntry;

    fn entry(id: &str, deps: &[&str]) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            directory: "/tmp".into(),
            command: "echo".into(),
            tag: "#000".into(),
            port: None,
            ready: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_apps() {
        let levels = topo_levels(&[]).unwrap();
        assert!(levels.is_empty());
    }

    #[test]
    fn single_no_deps() {
        let levels = topo_levels(&[entry("a", &[])]).unwrap();
        assert_eq!(levels, vec![vec!["a".to_string()]]);
    }

    #[test]
    fn linear_chain() {
        // a → b → c (c depends on b, b on a)
        let apps = vec![entry("a", &[]), entry("b", &["a"]), entry("c", &["b"])];
        let levels = topo_levels(&apps).unwrap();
        assert_eq!(
            levels,
            vec![
                vec!["a".to_string()],
                vec!["b".to_string()],
                vec!["c".to_string()],
            ]
        );
    }

    #[test]
    fn diamond() {
        //   a
        //  / \
        // b   c
        //  \ /
        //   d
        let apps = vec![
            entry("a", &[]),
            entry("b", &["a"]),
            entry("c", &["a"]),
            entry("d", &["b", "c"]),
        ];
        let levels = topo_levels(&apps).unwrap();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a"]);
        assert_eq!(levels[1], vec!["b", "c"]);
        assert_eq!(levels[2], vec!["d"]);
    }

    #[test]
    fn independent_islands() {
        let apps = vec![
            entry("a", &[]),
            entry("b", &[]),
            entry("c", &["b"]),
            entry("d", &[]),
        ];
        let levels = topo_levels(&apps).unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0], vec!["a", "b", "d"]);
        assert_eq!(levels[1], vec!["c"]);
    }

    #[test]
    fn cycle_of_two() {
        let apps = vec![entry("a", &["b"]), entry("b", &["a"])];
        let err = topo_levels(&apps).unwrap_err();
        match err {
            GraphError::Cycle(c) => {
                assert!(c.len() >= 3, "expected cycle path, got {c:?}");
                assert_eq!(c.first(), c.last(), "cycle should close on itself");
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn cycle_of_three() {
        let apps = vec![
            entry("a", &["c"]),
            entry("b", &["a"]),
            entry("c", &["b"]),
        ];
        let err = topo_levels(&apps).unwrap_err();
        match err {
            GraphError::Cycle(c) => {
                let names: HashSet<&str> = c.iter().map(String::as_str).collect();
                assert!(names.contains("a") && names.contains("b") && names.contains("c"));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn self_loop_rejected() {
        let apps = vec![entry("a", &["a"])];
        let err = topo_levels(&apps).unwrap_err();
        assert!(matches!(err, GraphError::SelfLoop(ref id) if id == "a"));
    }

    #[test]
    fn unknown_dep_rejected() {
        let apps = vec![entry("a", &["ghost"])];
        let err = topo_levels(&apps).unwrap_err();
        match err {
            GraphError::UnknownDep { app, dep } => {
                assert_eq!(app, "a");
                assert_eq!(dep, "ghost");
            }
            other => panic!("expected UnknownDep, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_dep_collapses() {
        // depends_on: [a, a] should be treated as a single edge — same as [a].
        let apps = vec![entry("a", &[]), entry("b", &["a", "a"])];
        let levels = topo_levels(&apps).unwrap();
        assert_eq!(
            levels,
            vec![vec!["a".to_string()], vec!["b".to_string()]]
        );
    }

    #[test]
    fn cycle_message_uses_arrows() {
        let err = topo_levels(&[entry("a", &["b"]), entry("b", &["a"])]).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("→"), "expected arrows in: {s}");
        assert!(s.contains("cycle"), "expected 'cycle' in: {s}");
    }
}
