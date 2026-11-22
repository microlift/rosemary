use super::cfg::{ControlFlow, Merge, loop_region, reaches, successors_of};
use super::*;
use crate::ir::Dominance;

/// Give a function a single returning block.
///
/// A Metal shader commonly returns from both sides of a bounds test. SPIR-V
/// needs every conditional branch to name the block where its paths rejoin,
/// and two returning blocks have none, so route both through one exit.
pub(super) fn single_exit(mut function: Function) -> Function {
    let returning: Vec<_> = function
        .blocks
        .iter()
        .filter(|block| matches!(block.terminator, Terminator::Return(_)))
        .map(|block| block.id)
        .collect();
    if returning.len() < 2 {
        return function;
    }
    let exit = BlockId(
        function
            .blocks
            .iter()
            .map(|block| block.id.0)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let mut incomings = Vec::new();
    for block in &mut function.blocks {
        let Terminator::Return(value) = block.terminator.clone() else {
            continue;
        };
        if let Some(value) = value {
            incomings.push((block.id, value));
        }
        block.terminator = Terminator::Branch(exit);
    }
    let (instructions, terminator) = if incomings.is_empty() {
        (Vec::new(), Terminator::Return(None))
    } else {
        let result = ValueId(function.values.len() as u32);
        function.values.push(function.result.clone());
        (
            vec![crate::ir::Instruction {
                result: Some(result),
                kind: InstructionKind::Phi { incomings },
            }],
            Terminator::Return(Some(Operand::Value(result))),
        )
    };
    function.blocks.push(crate::ir::Block {
        id: exit,
        name: "exit".to_string(),
        instructions,
        terminator,
    });
    function
}

/// Order blocks so that a block always precedes those it dominates, which
/// SPIR-V requires of a function's block list.
pub(super) fn reverse_post_order(function: &mut Function) {
    let graph = ControlFlow::new(function);
    let entry = function.blocks[0].id;
    let mut order = Vec::new();
    let mut visited = BTreeSet::new();
    let mut stack = vec![(entry, 0usize)];
    while let Some((block, index)) = stack.pop() {
        if index == 0 && !visited.insert(block) {
            continue;
        }
        match graph.successors[&block].get(index) {
            Some(successor) => {
                let successor = *successor;
                stack.push((block, index + 1));
                if !visited.contains(&successor) {
                    stack.push((successor, 0));
                }
            }
            None => order.push(block),
        }
    }
    order.reverse();
    // Unreachable blocks keep their original relative position at the end.
    let rank = |id: BlockId| {
        order
            .iter()
            .position(|block| *block == id)
            .unwrap_or(usize::MAX)
    };
    function.blocks.sort_by_key(|block| rank(block.id));
}

/// How many restructuring passes a function is allowed before its control flow
/// is declared beyond this translator.
pub(super) fn limit(function: &Function) -> usize {
    function.blocks.len().max(1)
}

/// Duplicate a path which a nested selection shares with its enclosing one.
///
/// In `if (a || b)`, the true edge of the `b` test enters the outer true arm.
/// That arm is not dominated by the inner header, so the inner construct has
/// no region of its own to merge. Clone the shared path for the inner edge;
/// `split_shared_merges` can then give the two constructs distinct merge
/// blocks. This is ordinary node splitting, with SSA definitions and phi
/// inputs cloned alongside the blocks which own them.
pub(super) fn isolate_shared_selection_paths(
    function: &mut Function,
) -> Result<bool, TranslateError> {
    let graph = ControlFlow::new(function);
    let entry = function.blocks[0].id;
    let dominators = graph.dominance(&[entry], &graph.predecessors);
    let merges = control_flow_merges(function)?;
    let structural = graph.structural_dominance(entry, &merges);
    for header in merges.keys().copied().collect::<BTreeSet<_>>() {
        let Merge::Selection { merge } = merges[&header] else {
            continue;
        };
        let inside: BTreeSet<_> = graph
            .blocks
            .iter()
            .copied()
            .filter(|block| {
                structural.dominates(header, *block) && !structural.dominates(merge, *block)
            })
            .collect();
        let redirects: Vec<_> = graph
            .blocks
            .iter()
            .filter(|block| !inside.contains(block))
            .flat_map(|source| {
                graph.successors[source]
                    .iter()
                    .copied()
                    .map(|target| (*source, target))
            })
            .filter(|(_, target)| *target != header && inside.contains(target))
            .collect();
        if redirects.is_empty() {
            continue;
        }
        let mut region = BTreeSet::new();
        let mut pending: Vec<_> = redirects.iter().map(|(_, target)| *target).collect();
        while let Some(block) = pending.pop() {
            if !inside.contains(&block) || !region.insert(block) {
                continue;
            }
            pending.extend(graph.successors[&block].iter().copied());
        }
        clone_region(function, &region, &redirects)?;
        return Ok(true);
    }
    let mut claims: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();
    for (header, merge) in &merges {
        if let Merge::Selection { merge } = merge {
            claims.entry(*merge).or_default().push(*header);
        }
    }
    for (merge, headers) in claims {
        if headers.len() < 2 {
            continue;
        }
        let inner = headers
            .iter()
            .copied()
            .max_by_key(|header| {
                headers
                    .iter()
                    .filter(|other| dominators.dominates(**other, *header))
                    .count()
            })
            .ok_or_else(|| emission("a shared merge block has no inner header"))?;
        let mut redirects = Vec::new();
        for block in &graph.blocks {
            if !dominators.dominates(inner, *block) {
                continue;
            }
            for successor in &graph.successors[block] {
                if *successor != merge && !dominators.dominates(inner, *successor) {
                    redirects.push((*block, *successor));
                }
            }
        }
        if redirects.is_empty() {
            continue;
        }
        let mut region = BTreeSet::new();
        let mut pending: Vec<_> = redirects.iter().map(|(_, target)| *target).collect();
        while let Some(block) = pending.pop() {
            if block == merge || !region.insert(block) {
                continue;
            }
            pending.extend(graph.successors[&block].iter().copied());
        }
        clone_region(function, &region, &redirects)?;
        return Ok(true);
    }
    Ok(false)
}

fn clone_region(
    function: &mut Function,
    region: &BTreeSet<BlockId>,
    redirects: &[(BlockId, BlockId)],
) -> Result<(), TranslateError> {
    let exits: BTreeSet<_> = function
        .blocks
        .iter()
        .filter(|block| region.contains(&block.id))
        .flat_map(|block| successors_of(&block.terminator))
        .filter(|target| !region.contains(target))
        .collect();
    let mut next_block = function
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .unwrap_or(0)
        + 1;
    let block_map: HashMap<_, _> = region
        .iter()
        .map(|block| {
            let cloned = BlockId(next_block);
            next_block += 1;
            (*block, cloned)
        })
        .collect();
    let mut value_map = HashMap::new();
    for block in function
        .blocks
        .iter()
        .filter(|block| region.contains(&block.id))
    {
        for instruction in &block.instructions {
            if let Some(value) = instruction.result {
                let cloned = ValueId(function.values.len() as u32);
                function
                    .values
                    .push(function.values[value.0 as usize].clone());
                value_map.insert(value, cloned);
            }
        }
    }
    let originals: Vec<_> = function
        .blocks
        .iter()
        .filter(|block| region.contains(&block.id))
        .cloned()
        .collect();
    let redirected: BTreeSet<_> = redirects.iter().copied().collect();
    let mut clones = Vec::new();
    for mut block in originals {
        let original_id = block.id;
        block.id = block_map[&original_id];
        block.name = format!("{}_clone{}", block.name, block.id.0);
        for instruction in &mut block.instructions {
            if let Some(result) = &mut instruction.result {
                *result = value_map[result];
            }
            if let InstructionKind::Phi { incomings } = &mut instruction.kind {
                incomings.retain(|(from, _)| {
                    region.contains(from) || redirected.contains(&(*from, original_id))
                });
                for (from, _) in incomings.iter_mut() {
                    if let Some(cloned) = block_map.get(from) {
                        *from = *cloned;
                    }
                }
            }
            for operand in instruction_operands_mut(&mut instruction.kind) {
                remap_value(operand, &value_map);
            }
        }
        remap_terminator(&mut block.terminator, &block_map, &value_map);
        clones.push(block);
    }

    // The original entries no longer receive the redirected edges.
    for block in &mut function.blocks {
        for instruction in &mut block.instructions {
            if let InstructionKind::Phi { incomings } = &mut instruction.kind {
                incomings.retain(|(from, _)| !redirected.contains(&(*from, block.id)));
            }
        }
    }
    // Every cloned exit contributes its own SSA values at the boundary.
    for exit in exits {
        let merge_block = function
            .blocks
            .iter_mut()
            .find(|block| block.id == exit)
            .ok_or_else(|| emission("a cloned region's merge block is missing"))?;
        for instruction in &mut merge_block.instructions {
            let InstructionKind::Phi { incomings } = &mut instruction.kind else {
                continue;
            };
            let additions: Vec<_> = incomings
                .iter()
                .filter_map(|(from, value)| {
                    block_map.get(from).map(|cloned| {
                        let mut value = value.clone();
                        remap_value(&mut value, &value_map);
                        (*cloned, value)
                    })
                })
                .collect();
            incomings.extend(additions);
        }
    }
    for (source, target) in redirects {
        let block = function
            .blocks
            .iter_mut()
            .find(|block| block.id == *source)
            .ok_or_else(|| emission("a cloned region's incoming block is missing"))?;
        redirect_edge(&mut block.terminator, *target, block_map[target]);
    }
    function.blocks.extend(clones);
    Ok(())
}

fn remap_value(operand: &mut Operand, values: &HashMap<ValueId, ValueId>) {
    if let Operand::Value(value) = operand
        && let Some(cloned) = values.get(value)
    {
        *value = *cloned;
    }
}

fn remap_terminator(
    terminator: &mut Terminator,
    blocks: &HashMap<BlockId, BlockId>,
    values: &HashMap<ValueId, ValueId>,
) {
    match terminator {
        Terminator::Branch(target) => {
            if let Some(cloned) = blocks.get(target) {
                *target = *cloned;
            }
        }
        Terminator::CondBranch {
            condition,
            true_block,
            false_block,
        } => {
            remap_value(condition, values);
            if let Some(cloned) = blocks.get(true_block) {
                *true_block = *cloned;
            }
            if let Some(cloned) = blocks.get(false_block) {
                *false_block = *cloned;
            }
        }
        Terminator::Return(value) => {
            if let Some(value) = value {
                remap_value(value, values);
            }
        }
        Terminator::Unreachable => {}
    }
}

fn redirect_edge(terminator: &mut Terminator, from: BlockId, to: BlockId) {
    match terminator {
        Terminator::Branch(target) if *target == from => *target = to,
        Terminator::CondBranch {
            true_block,
            false_block,
            ..
        } => {
            if *true_block == from {
                *true_block = to;
            }
            if *false_block == from {
                *false_block = to;
            }
        }
        _ => {}
    }
}

/// Give each structured header its own merge block.
///
/// An `else if` chain reaches one rejoining block from several conditional
/// branches, and SPIR-V forbids two headers from naming the same merge. Route
/// the inner construct's edges through a new block, which then becomes its
/// merge.
pub(super) fn split_shared_merges(function: &mut Function) -> Result<bool, TranslateError> {
    let merges = control_flow_merges(function)?;
    let graph = ControlFlow::new(function);
    let dominators = graph.dominance(&[function.blocks[0].id], &graph.predecessors);
    for header in merges.keys().copied().collect::<BTreeSet<_>>() {
        let Merge::Selection { merge } = merges[&header] else {
            continue;
        };
        if dominators.dominates(header, merge) {
            continue;
        }
        // A path the header shares with another construct reaches the merge
        // through a block the header does not dominate. A new block on the
        // dominated edges would not post-dominate the header, so it could
        // never become the merge; the shared path has to be cloned first.
        if !merge_entries_dominated(&graph, &dominators, header, merge) {
            continue;
        }
        let sources: Vec<_> = graph
            .predecessors
            .get(&merge)
            .into_iter()
            .flatten()
            .copied()
            .filter(|source| dominators.dominates(header, *source))
            .collect();
        if sources.is_empty() {
            return Err(emission(
                "a selection merge has no incoming edge dominated by its header",
            ));
        }
        split_merge(function, merge, &sources);
        return Ok(true);
    }
    let mut claims: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();
    for (header, merge) in &merges {
        let block = match merge {
            Merge::Selection { merge } | Merge::Loop { merge, .. } => *merge,
        };
        claims.entry(block).or_default().push(*header);
    }
    let Some((merge, headers)) = claims.into_iter().find(|(_, headers)| headers.len() > 1) else {
        return Ok(false);
    };
    // The innermost header is the one every other claimant dominates.
    let inner = headers
        .iter()
        .copied()
        .max_by_key(|header| {
            headers
                .iter()
                .filter(|other| dominators.dominates(**other, *header))
                .count()
        })
        .ok_or_else(|| emission("a merge block has no claiming header"))?;
    let sources: Vec<BlockId> = graph
        .predecessors
        .get(&merge)
        .into_iter()
        .flatten()
        .copied()
        .filter(|predecessor| dominators.dominates(inner, *predecessor))
        .collect();
    if sources.is_empty() || sources.len() == graph.predecessors[&merge].len() {
        return Err(emission(
            "a shared merge block cannot be split into nested constructs",
        ));
    }
    split_merge(function, merge, &sources);
    Ok(true)
}

/// Whether every edge by which `header`'s paths reach `merge` comes from a
/// block `header` dominates.
fn merge_entries_dominated(
    graph: &ControlFlow,
    dominators: &Dominance,
    header: BlockId,
    merge: BlockId,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut pending = vec![header];
    while let Some(block) = pending.pop() {
        if block == merge || !seen.insert(block) {
            continue;
        }
        for successor in &graph.successors[&block] {
            if *successor == merge && !dominators.dominates(header, block) {
                return false;
            }
            pending.push(*successor);
        }
    }
    true
}

/// Give a loop whose body rejoins at its latch an empty continue target.
///
/// SPIR-V's loop construct does not contain the continue construct, so no
/// construct inside the body may merge at the continue target. Two shapes
/// reach it: an `if`/`else` at the end of a loop body, which rejoins at
/// exactly that block, and an inner loop the body leaves by continuing the
/// outer one, whose only exit is that block. Both are answered the same way -
/// route the back edge through a new block, leaving the old latch in the body
/// where it is a legal merge.
pub(super) fn separate_loop_latches(function: &mut Function) -> Result<bool, TranslateError> {
    let mut changed = false;
    for _ in 0..limit(function) {
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
            let [continue_target] = latches.as_slice() else {
                continue;
            };
            let Some((body, exit)) = loop_region(
                &graph,
                &dominators,
                &post_dominators,
                block.id,
                *continue_target,
            ) else {
                continue;
            };
            loops.push((block.id, body, *continue_target, exit));
        }
        let mut split = None;
        // An inner loop the body leaves by continuing the outer one has the
        // outer continue target as its merge, which lies outside the outer
        // loop construct.
        for (inner, inner_body, _, exit) in &loops {
            let enclosing = loops
                .iter()
                .filter(|(header, body, ..)| header != inner && body.contains(inner))
                .min_by_key(|(_, body, ..)| body.len());
            let Some((header, _, continue_target, _)) = enclosing else {
                continue;
            };
            if exit == continue_target && !inner_body.contains(continue_target) {
                split = Some((*header, *continue_target));
                break;
            }
        }
        for block in &function.blocks {
            if split.is_some() {
                break;
            }
            if !matches!(block.terminator, Terminator::CondBranch { .. }) {
                continue;
            }
            if loops.iter().any(|(header, ..)| *header == block.id) {
                continue;
            }
            let Some((header, body, continue_target, _)) = loops
                .iter()
                .filter(|(_, body, ..)| body.contains(&block.id))
                .min_by_key(|(_, body, ..)| body.len())
            else {
                continue;
            };
            if post_dominators.immediate(block.id) != Some(*continue_target) {
                continue;
            }
            // The emitter merges such a conditional at the arm which keeps
            // running the body. When both arms do, or neither does, there is
            // no such arm, and the latch is where they rejoin.
            let staying = successors_of(&block.terminator)
                .into_iter()
                .filter(|successor| successor != continue_target)
                .filter(|successor| reaches(&graph, *successor, *continue_target, body))
                .count();
            if staying == 1 {
                continue;
            }
            split = Some((*header, *continue_target));
            break;
        }
        let Some((header, latch)) = split else {
            return Ok(changed);
        };
        split_merge(function, header, &[latch]);
        changed = true;
    }
    Err(emission(
        "a loop's latch could not be separated from its body",
    ))
}

/// Give a loop a merge block of its own where the block it leaves to is also
/// reached from outside it.
///
/// A loop's merge must be dominated by its header. A block the body leaves to
/// which something outside the loop also branches to is not, so the exit edges
/// are routed through a new block, which is - and which becomes the merge,
/// because it is now the only block the body leaves to.
pub(super) fn isolate_loop_merges(function: &mut Function) -> Result<bool, TranslateError> {
    let mut changed = false;
    for _ in 0..limit(function) {
        let graph = ControlFlow::new(function);
        let entry = function.blocks[0].id;
        let dominators = graph.dominance(&[entry], &graph.predecessors);
        let post_dominators = graph.post_dominance();
        let mut split = None;
        for block in &function.blocks {
            let latches: Vec<_> = graph
                .predecessors
                .get(&block.id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|predecessor| dominators.dominates(block.id, *predecessor))
                .collect();
            let [continue_target] = latches.as_slice() else {
                continue;
            };
            let Some((body, merge)) = loop_region(
                &graph,
                &dominators,
                &post_dominators,
                block.id,
                *continue_target,
            ) else {
                continue;
            };
            if dominators.dominates(block.id, merge) {
                continue;
            }
            let inside: Vec<_> = graph
                .predecessors
                .get(&merge)
                .into_iter()
                .flatten()
                .copied()
                .filter(|predecessor| body.contains(predecessor))
                .collect();
            split = Some((merge, inside));
            break;
        }
        let Some((merge, inside)) = split else {
            return Ok(changed);
        };
        split_merge(function, merge, &inside);
        changed = true;
    }
    Err(emission(
        "a loop's merge could not be separated from the code which reaches it from outside",
    ))
}

/// Give a loop whose header also chooses between two body paths a header of
/// its own.
///
/// A block declares one merge instruction. When a loop header's conditional
/// branch goes to two blocks inside the body, the header would have to be both
/// the loop's header and the selection's, and SPIR-V has no way to say so.
/// Routing every edge into the header through a new block leaves that block
/// carrying the loop and the old header carrying the selection.
pub(super) fn normalize_loop_headers(function: &mut Function) -> Result<bool, TranslateError> {
    let mut changed = false;
    for _ in 0..limit(function) {
        let graph = ControlFlow::new(function);
        let entry = function.blocks[0].id;
        let dominators = graph.dominance(&[entry], &graph.predecessors);
        let post_dominators = graph.post_dominance();
        let mut split = None;
        for block in &function.blocks {
            if !matches!(block.terminator, Terminator::CondBranch { .. }) {
                continue;
            }
            let predecessors: Vec<_> = graph
                .predecessors
                .get(&block.id)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            let latches: Vec<_> = predecessors
                .iter()
                .copied()
                .filter(|predecessor| dominators.dominates(block.id, *predecessor))
                .collect();
            let [continue_target] = latches.as_slice() else {
                continue;
            };
            let Some((_, merge)) = loop_region(
                &graph,
                &dominators,
                &post_dominators,
                block.id,
                *continue_target,
            ) else {
                continue;
            };
            // A header which leaves the loop or reaches its latch directly is
            // already the loop's own test and needs no separate block.
            if successors_of(&block.terminator)
                .iter()
                .any(|successor| *successor == merge || successor == continue_target)
            {
                continue;
            }
            split = Some((block.id, predecessors));
            break;
        }
        let Some((header, predecessors)) = split else {
            return Ok(changed);
        };
        split_merge(function, header, &predecessors);
        changed = true;
    }
    Err(emission(
        "a loop header could not be separated from the selection it also makes",
    ))
}

/// Move every phi to the head of its block.
///
/// SPIR-V requires it, and restructuring adds phis to blocks which already
/// have instructions. A phi only names values from its predecessors, so its
/// position within its own block never changes what it computes.
pub(super) fn hoist_phis(function: &mut Function) {
    for block in &mut function.blocks {
        let instructions = std::mem::take(&mut block.instructions);
        let (phis, rest): (Vec<_>, Vec<_>) = instructions
            .into_iter()
            .partition(|instruction| matches!(instruction.kind, InstructionKind::Phi { .. }));
        block.instructions = phis;
        block.instructions.extend(rest);
    }
}

/// Insert a block between `sources` and `merge`, moving the phi inputs which
/// arrive from those predecessors into it.
fn split_merge(function: &mut Function, merge: BlockId, sources: &[BlockId]) {
    let inserted = BlockId(
        function
            .blocks
            .iter()
            .map(|block| block.id.0)
            .max()
            .unwrap_or(0)
            + 1,
    );
    for block in &mut function.blocks {
        if !sources.contains(&block.id) {
            continue;
        }
        block.terminator = match block.terminator.clone() {
            Terminator::Branch(target) if target == merge => Terminator::Branch(inserted),
            Terminator::CondBranch {
                condition,
                true_block,
                false_block,
            } => Terminator::CondBranch {
                condition,
                true_block: if true_block == merge {
                    inserted
                } else {
                    true_block
                },
                false_block: if false_block == merge {
                    inserted
                } else {
                    false_block
                },
            },
            other => other,
        };
    }
    let mut instructions = Vec::new();
    let merge_block = function
        .blocks
        .iter()
        .position(|block| block.id == merge)
        .expect("the merge block exists");
    let mut phis = std::mem::take(&mut function.blocks[merge_block].instructions);
    for instruction in &mut phis {
        let InstructionKind::Phi { incomings } = &mut instruction.kind else {
            continue;
        };
        let (inner, outer): (Vec<_>, Vec<_>) = incomings
            .iter()
            .cloned()
            .partition(|(from, _)| sources.contains(from));
        let replacement = match inner.as_slice() {
            [] => continue,
            [(_, value)] => value.clone(),
            _ => {
                let result = ValueId(function.values.len() as u32);
                let ty = instruction
                    .result
                    .and_then(|value| function.values.get(value.0 as usize))
                    .cloned()
                    .unwrap_or(Type::Void);
                function.values.push(ty);
                instructions.push(crate::ir::Instruction {
                    result: Some(result),
                    kind: InstructionKind::Phi { incomings: inner },
                });
                Operand::Value(result)
            }
        };
        *incomings = outer;
        incomings.push((inserted, replacement));
    }
    function.blocks[merge_block].instructions = phis;
    function.blocks.push(crate::ir::Block {
        id: inserted,
        name: format!("merge{}", inserted.0),
        instructions,
        terminator: Terminator::Branch(merge),
    });
}
