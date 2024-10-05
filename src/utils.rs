use petgraph::{
    algo::kosaraju_scc,
    stable_graph::{NodeIndex, StableGraph},
    visit::{EdgeRef, IntoEdgeReferences, NodeFiltered},
};
use std::collections::{HashMap, HashSet};

pub fn merge_graphs<N: Clone, E: Clone>(
    g: &StableGraph<N, E>,
    h: &StableGraph<N, E>,
) -> (StableGraph<N, E>, HashMap<NodeIndex, NodeIndex>) {
    let mut total = g.clone();
    let mut nx_map = HashMap::new();

    for nx in h.node_indices() {
        let new_nx = total.add_node(h[nx].clone());
        nx_map.insert(nx, new_nx);
    }

    for edge in h.edge_references() {
        let nx_a = edge.source();
        let nx_b = edge.target();
        let weight = edge.weight().clone();

        let new_nx_a = nx_map[&nx_a];
        let new_nx_b = nx_map[&nx_b];
        total.add_edge(new_nx_a, new_nx_b, weight);
    }

    (total, nx_map)
}

pub fn split_graph_components<N: Clone, E: Clone>(g: StableGraph<N, E>) -> Vec<StableGraph<N, E>> {
    let ccs = kosaraju_scc(&g);
    if ccs.len() == 1 {
        return vec![g];
    }

    let mut cc_graphs = Vec::new();
    let mut nx_map = HashMap::new();
    let mut nx_to_cc: HashMap<NodeIndex, usize> = HashMap::new();

    for (i, cc) in ccs.into_iter().enumerate() {
        nx_map.clear();
        let cc: HashSet<NodeIndex> = HashSet::from_iter(cc.into_iter());
        let mut cc_graph = StableGraph::new();
        for &nx in &cc {
            nx_to_cc.insert(nx, i);
            nx_map.insert(nx, cc_graph.add_node(g[nx].clone()));
        }

        let node_filter = NodeFiltered::from_fn(&g, |n| cc.contains(&n));

        for edge in node_filter.edge_references() {
            cc_graph.add_edge(
                nx_map[&edge.source()],
                nx_map[&edge.target()],
                edge.weight().clone(),
            );
        }

        cc_graphs.push(cc_graph);
    }

    cc_graphs
}
