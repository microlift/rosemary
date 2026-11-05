use std::collections::HashMap;

use super::BlockId;

/// A dominator tree: each block's immediate dominator, with a root naming
/// itself, and every block's span in a walk of the tree so that a query is a
/// comparison rather than a climb.
pub(crate) struct Dominance {
    idom: HashMap<BlockId, BlockId>,
    /// Entry and exit ordinals of a depth-first walk of the tree: a block
    /// dominates exactly the blocks whose span its own encloses.
    spans: HashMap<BlockId, (u32, u32)>,
}

impl Dominance {
    /// Immediate dominators, by the Cooper-Harvey-Kennedy iteration, used for
    /// both dominators and post-dominators by swapping the edge direction.
    ///
    /// `edges` names each block's incoming edges for the chosen direction, so
    /// the walk itself follows their reverse. A block no root reaches has no
    /// dominator but itself. A dominator tree rather than a set per block: a
    /// shader whose entry point inlines to a few thousand blocks makes the set
    /// form quadratic in both time and memory.
    pub(crate) fn new(roots: &[BlockId], edges: &HashMap<BlockId, Vec<BlockId>>) -> Self {
        let mut forward: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
        for (block, incoming) in edges {
            for source in incoming {
                forward.entry(*source).or_default().push(*block);
            }
        }
        // Reverse post-order, so that a block is visited after the blocks
        // whose dominators it depends on wherever the graph is acyclic.
        let mut visited = std::collections::BTreeSet::new();
        let mut post = Vec::new();
        for root in roots {
            if !visited.insert(*root) {
                continue;
            }
            let mut stack = vec![(*root, 0usize)];
            while let Some((block, index)) = stack.pop() {
                let next = forward.get(&block).map(|s| s.as_slice()).unwrap_or(&[]);
                match next.get(index) {
                    Some(successor) => {
                        stack.push((block, index + 1));
                        if visited.insert(*successor) {
                            stack.push((*successor, 0));
                        }
                    }
                    None => post.push(block),
                }
            }
        }
        post.reverse();
        let order: HashMap<BlockId, usize> = post
            .iter()
            .enumerate()
            .map(|(index, block)| (*block, index))
            .collect();

        let mut idom: HashMap<BlockId, BlockId> = roots.iter().map(|root| (*root, *root)).collect();
        loop {
            let mut changed = false;
            for block in &post {
                if roots.contains(block) {
                    continue;
                }
                let mut candidate = None;
                for source in edges.get(block).into_iter().flatten() {
                    if !idom.contains_key(source) {
                        continue;
                    }
                    candidate = Some(match candidate {
                        None => *source,
                        // Two paths which meet at no common block leave this
                        // one dominated by nothing but itself, which is what
                        // its own identity records.
                        Some(current) => match meet(*source, current, &idom, &order) {
                            Some(meet) => meet,
                            None => *block,
                        },
                    });
                }
                let candidate = candidate.unwrap_or(*block);
                if idom.insert(*block, candidate) != Some(candidate) {
                    changed = true;
                }
            }
            if !changed {
                let spans = spans(&post, &idom);
                return Self { idom, spans };
            }
        }
    }

    /// Reflexive, as a dominator set which contains its own block is.
    pub(crate) fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        if dominator == block {
            return true;
        }
        match (self.spans.get(&dominator), self.spans.get(&block)) {
            (Some(outer), Some(inner)) => outer.0 < inner.0 && inner.1 < outer.1,
            _ => false,
        }
    }

    /// The closest strict dominator, or `None` for a root and for a block
    /// whose paths in are dominated by nothing they share.
    pub(crate) fn immediate(&self, block: BlockId) -> Option<BlockId> {
        self.idom
            .get(&block)
            .copied()
            .filter(|dominator| *dominator != block)
    }
}

/// Number each block on entry to and exit from its subtree of the dominator
/// tree. Every block of `order` is in `idom`, and every tree is rooted at a
/// block which names itself.
fn spans(order: &[BlockId], idom: &HashMap<BlockId, BlockId>) -> HashMap<BlockId, (u32, u32)> {
    let mut children: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    let mut tree_roots = Vec::new();
    // In reverse post-order, so the numbering does not depend on hash order.
    for block in order {
        match idom.get(block) {
            Some(parent) if parent != block => children.entry(*parent).or_default().push(*block),
            _ => tree_roots.push(*block),
        }
    }
    let mut spans = HashMap::with_capacity(order.len());
    let mut clock = 0u32;
    for root in tree_roots {
        let mut stack = vec![(root, 0usize)];
        spans.insert(root, (clock, 0));
        clock += 1;
        while let Some((block, index)) = stack.pop() {
            match children.get(&block).and_then(|below| below.get(index)) {
                Some(child) => {
                    stack.push((block, index + 1));
                    spans.insert(*child, (clock, 0));
                    clock += 1;
                    stack.push((*child, 0));
                }
                None => {
                    spans.get_mut(&block).expect("entered above").1 = clock;
                    clock += 1;
                }
            }
        }
    }
    spans
}

/// The closest block both `a` and `b` are dominated by, or `None` when they
/// are reached from separate roots and share none.
fn meet(
    mut a: BlockId,
    mut b: BlockId,
    idom: &HashMap<BlockId, BlockId>,
    order: &HashMap<BlockId, usize>,
) -> Option<BlockId> {
    while a != b {
        let deeper = match order.get(&a)? > order.get(&b)? {
            true => &mut a,
            false => &mut b,
        };
        let next = *idom.get(deeper)?;
        if next == *deeper {
            // A root, and the other block is not below it.
            return None;
        }
        *deeper = next;
    }
    Some(a)
}
