use petgraph::algo::toposort;
use petgraph::stable_graph::{EdgeIndex, NodeIndex, StableGraph};
use petgraph::visit::{EdgeFiltered, EdgeRef};
use petgraph::{data, Direction, Graph};
use slotmap::{new_key_type, DefaultKey, SecondaryMap, SlotMap};
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::iter::{self, Zip};

new_key_type! {
    struct CompKey;
    struct NetKey;
}

#[derive(Debug, Default)]
struct Circuit {
    // components: SlotMap<CompKey, Component>,
    nets: SlotMap<NetKey, Net>,
    graph: StableGraph<Component, NetKey>,
}

impl Circuit {
    fn add_component(&mut self, comp: Component) -> NodeIndex {
        self.graph.add_node(comp)
    }

    fn remove_component(&mut self, idx: NodeIndex) -> Option<Component> {
        self.graph.remove_node(idx)
    }

    fn add_net(&mut self, net: Net) -> Option<NetKey> {
        net.input.map(|input| {
            self.nets.insert_with_key(|key| {
                for output in &net.outputs {
                    self.graph.add_edge(input, *output, key);
                }
                net
            })
        })
    }

    fn remove_net(&mut self, net_key: NetKey) -> Option<Net> {
        self.nets.remove(net_key).map(|net| {
            let edges_to_remove: Vec<_> = self
                .graph
                .edge_indices()
                .filter(|ex| self.graph[*ex] == net_key)
                .collect();
            for ex in edges_to_remove {
                self.graph.remove_edge(ex);
            }
            net
        })
    }

    fn feed_forward(&mut self) {
        // ignore clocked components to avoid infinite loops
        let de_cycled =
            EdgeFiltered::from_fn(&self.graph, |e| !self.graph[e.target()].kind.is_clocked());
        // get the directed order to update the rest of the components
        let order = toposort(&de_cycled, None).expect("No cycles in unclocked components.");
        self.evaluate_components(order);
    }

    fn tick_clock(&mut self) {
        let clocked_order: Vec<_> = self
            .graph
            .node_indices()
            .filter(|cx| self.graph[*cx].kind.is_clocked())
            .collect();
        self.evaluate_components(clocked_order);
        self.feed_forward();
    }

    fn evaluate_components(&mut self, order: Vec<NodeIndex>) {
        for cx in order {
            let comp = &self.graph[cx];
            let input_signals: Vec<_> = comp
                .inputs
                .iter()
                .map(|pin| self.nets.get(pin.net).unwrap().signal)
                .collect();
            let output_signals = comp.evaluate(&input_signals);
            assert_eq!(output_signals.len(), comp.outputs.len());
            for (output_signal, pin) in std::iter::zip(output_signals, &comp.outputs) {
                let net = self.nets.get_mut(pin.net).unwrap();
                net.signal = if output_signal.bits == pin.bits {
                    output_signal
                } else {
                    Signal {
                        value: None,
                        ..output_signal
                    }
                };
            }
        }
    }
}

#[derive(Debug)]
struct Component {
    kind: CompKind,
    inputs: Vec<Pin>,
    outputs: Vec<Pin>,
}

impl Component {
    pub fn evaluate(&self, inputs: &[Signal]) -> Vec<Signal> {
        self.kind.evaluate(inputs)
    }
}

#[derive(Debug)]
enum CompKind {
    Input(Input),
    Output(Output),
    Gate(Gate),
    Reg(Reg),
}

impl CompKind {
    pub fn is_clocked(&self) -> bool {
        match self {
            CompKind::Gate(_) => false,
            CompKind::Reg(_) => true,
            CompKind::Input(_) => false,
            CompKind::Output(_) => false,
        }
    }
    pub fn evaluate(&self, inputs: &[Signal]) -> Vec<Signal> {
        todo!()
    }
}

#[derive(Debug, Default)]
struct Net {
    bits: u8,
    signal: Signal,
    input: Option<NodeIndex>,
    outputs: Vec<NodeIndex>,
}

#[derive(Debug, Default, Copy, Clone)]
struct Signal {
    bits: u8,
    value: Option<u32>,
}

#[derive(Debug, Default, Copy, Clone)]
struct Pin {
    bits: u8,
    net: NetKey,
}
