//! # suite-nodes — the generic node editor + evaluation graph.
//!
//! **One spine, three faces** (materials / compositing / DSP). The structure, editor,
//! and evaluation are shared; the wire data type and the execution context differ per
//! app — and that last difference is load-bearing (the DAW evaluates lock-free under a
//! sub-ms deadline). Evaluation is **lazy pull + dirty-propagate + cache** — the same
//! discipline as the modifier stack and the adjustment-layer compositor. docs/03 §4.4–4.6.
//!
//! Status: stub.

#![allow(dead_code)]

/// A node in the evaluation graph. The `dirty` flag drives "re-evaluate only the dirty
/// sub-graph, reuse every clean cache." docs/03 §4.5.
pub struct Node {
    pub dirty: bool,
    // pub kind: NodeKind, pub params: ParamBlock,        // TODO(Phase 7)
    // pub inputs: Vec<Port>, pub outputs: Vec<Port>, pub cache: Option<Cached>,
}

/// The graph: nodes + type-checked wires. Kept acyclic; topo-sort for batch eval.
/// TODO(Phase 7).
pub struct Graph;
