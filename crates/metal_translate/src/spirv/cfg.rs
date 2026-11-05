use super::*;
use crate::ir::Dominance;

pub(super) struct ControlFlow {
    pub(super) blocks: Vec<BlockId>,
    pub(super) successors: HashMap<BlockId, Vec<BlockId>>,
    pub(super) predecessors: HashMap<BlockId, Vec<BlockId>>,
}

impl ControlFlow {
    pub(super) fn new(function: &Function) -> Self {
        let mut graph = Self {
            blocks: function.blocks.iter().map(|block| block.id).collect(),
            successors: HashMap::new(),
            predecessors: HashMap::new(),
        };
        for block in &function.blocks {
            let successors = successors_of(&block.terminator);
            for successor in &successors {
                graph
                    .predecessors
                    .entry(*successor)
                    .or_default()
                    .push(block.id);
            }
            graph.successors.insert(block.id, successors);
        }
        graph
    }

    /// See [`Dominance::new`].
    pub(super) fn dominance(
        &self,
        roots: &[BlockId],
        edges: &HashMap<BlockId, Vec<BlockId>>,
    ) -> Dominance {
        Dominance::new(roots, edges)
    }
}

/// Recover the structured constructs of a function's control-flow graph.
pub(super) fn control_flow_merges(
    function: &Function,
) -> Result<HashMap<BlockId, Merge>, TranslateError> {
    let graph = ControlFlow::new(function);
    let entry = function.blocks[0].id;
    let dominators = graph.dominance(&[entry], &graph.predecessors);
    let exits: Vec<_> = graph
        .blocks
        .iter()
        .copied()
        .filter(|block| graph.successors[block].is_empty())
        .collect();
    let post_dominators = graph.dominance(&exits, &graph.successors);
    // Loops first: a selection inside one may branch out of it, and that
    // choice depends on where the loop continues.
    let mut loops = Vec::new();
    for block in &function.blocks {
        let latches: Vec<_> = graph
            .predecessors
            .get(&block.id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|predecessor| dominators.dominates(block.id, *predecessor))
            .collect();
        if latches.is_empty() {
            continue;
        }
        let [continue_target] = latches.as_slice() else {
            return Err(emission(
                "a loop with several back edges needs restructuring",
            ));
        };
        let body = natural_loop(&graph, block.id, *continue_target);
        let mut leaving: Vec<_> = body
            .iter()
            .flat_map(|member| graph.successors[member].iter().copied())
            .filter(|successor| !body.contains(successor))
            .collect();
        leaving.sort_unstable();
        leaving.dedup();
        let [merge] = leaving.as_slice() else {
            return Err(emission("a loop with several exits needs restructuring"));
        };
        loops.push((block.id, body, *merge, *continue_target));
    }
    let mut merges = HashMap::new();
    for (header, _, merge, continue_target) in &loops {
        merges.insert(
            *header,
            Merge::Loop {
                merge: *merge,
                continue_target: *continue_target,
            },
        );
    }
    for block in &function.blocks {
        if !matches!(block.terminator, Terminator::CondBranch { .. })
            || merges.contains_key(&block.id)
        {
            continue;
        }
        let merge = post_dominators.immediate(block.id).ok_or_else(|| {
            emission("a conditional branch's paths never rejoin, which SPIR-V cannot express")
        })?;
        // A conditional which leaves an enclosing loop, whether by `break` or
        // by `continue`, has no rejoining block inside the loop body: its
        // paths meet only at the loop's merge or its continue target, and
        // SPIR-V accepts neither as a selection's merge. Use the arm which
        // keeps running the body instead.
        let enclosing = loops
            .iter()
            .filter(|(_, body, _, _)| body.contains(&block.id))
            .min_by_key(|(_, body, _, _)| body.len());
        let merge = match enclosing {
            Some((header, body, loop_merge, continue_target))
                if !body.contains(&merge) || merge == *continue_target =>
            {
                let successors = successors_of(&block.terminator);
                // A conditional whose two arms are the loop's own merge and
                // its continue target is a `break` against a `continue`, and
                // one in the continue target itself whose arms are the back
                // edge and the merge is a `do`/`while`'s test. Neither
                // declares a selection: SPIR-V lets a branch leave a construct
                // by a merge, a continue target or a back edge without one.
                let structureless = |successor: &BlockId| {
                    successor == loop_merge
                        || successor == continue_target
                        || (block.id == *continue_target && successor == header)
                };
                if successors.iter().all(structureless) {
                    continue;
                }
                let staying: Vec<_> = successors
                    .into_iter()
                    .filter(|successor| successor != continue_target)
                    .filter(|successor| reaches(&graph, *successor, *continue_target, body))
                    .collect();
                match staying.as_slice() {
                    [only] => *only,
                    // Two arms which both run more of the body before
                    // leaving it do not rejoin inside the loop. Expressing
                    // that needs real restructuring, not a merge choice.
                    _ => {
                        return Err(emission(
                            "a conditional which leaves its loop has no single continuing arm",
                        ));
                    }
                }
            }
            _ => merge,
        };
        merges.insert(block.id, Merge::Selection { merge });
    }
    Ok(merges)
}

/// Whether `target` is reachable from `from` without leaving `within`.
pub(super) fn reaches(
    graph: &ControlFlow,
    from: BlockId,
    target: BlockId,
    within: &BTreeSet<BlockId>,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut pending = vec![from];
    while let Some(block) = pending.pop() {
        if block == target {
            return true;
        }
        if !within.contains(&block) || !seen.insert(block) {
            continue;
        }
        pending.extend(graph.successors[&block].iter().copied());
    }
    false
}

/// The blocks of the natural loop formed by the back edge `latch -> header`.
pub(super) fn natural_loop(
    graph: &ControlFlow,
    header: BlockId,
    latch: BlockId,
) -> BTreeSet<BlockId> {
    let mut body = BTreeSet::from([header]);
    let mut pending = vec![latch];
    while let Some(block) = pending.pop() {
        if !body.insert(block) {
            continue;
        }
        pending.extend(
            graph
                .predecessors
                .get(&block)
                .into_iter()
                .flatten()
                .copied(),
        );
    }
    body
}

/// The structured construct a block with two successors introduces.
///
/// SPIR-V has no unstructured control flow: every conditional branch must
/// declare where its two paths rejoin, and a loop must additionally name its
/// back edge. LLVM records neither, so both are recovered from the CFG.
#[derive(Clone, Copy, Debug)]
pub(super) enum Merge {
    Selection {
        merge: BlockId,
    },
    Loop {
        merge: BlockId,
        continue_target: BlockId,
    },
}

pub(super) fn successors_of(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Branch(target) => vec![*target],
        Terminator::CondBranch {
            true_block,
            false_block,
            ..
        } => vec![*true_block, *false_block],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}
