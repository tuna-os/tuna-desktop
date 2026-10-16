use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
/// Resolve inherited flags in linear graph walks. Own flags remain separate,
/// so clearing a parent does not clear an independently flagged descendant.
/// Cycles inherit any flagged member and never recurse.
pub fn inherit_flags(
    parents: &HashMap<u64, u64>,
    flags: &HashSet<u64>,
    ids: impl IntoIterator<Item = u64>,
) -> HashSet<u64> {
    let mut resolved = HashMap::new();
    for id in ids {
        if resolved.contains_key(&id) {
            continue;
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut current = id;
        let mut inherited = loop {
            if let Some(value) = resolved.get(&current) {
                break *value;
            }
            if let Some(index) = positions.get(&current) {
                break path[*index..].iter().any(|id| flags.contains(id));
            }
            positions.insert(current, path.len());
            path.push(current);
            match parents.get(&current) {
                Some(parent) => current = *parent,
                None => break false,
            }
        };
        for id in path.into_iter().rev() {
            inherited |= flags.contains(&id);
            resolved.insert(id, inherited);
        }
    }
    resolved
        .into_iter()
        .filter_map(|(id, value)| value.then_some(id))
        .collect()
}
/// Selected window followed by each ancestor, without recursion or cycles.
pub fn ancestors(id: u64, parents: &HashMap<u64, u64>) -> Vec<u64> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut current = id;
    while seen.insert(current) {
        result.push(current);
        match parents.get(&current) {
            Some(parent) => current = *parent,
            None => break,
        }
    }
    result
}
/// Raise a transient family together while keeping every parent below its
/// descendants. Selected ancestors have lower queue priority, so unrelated
/// siblings rise first. Client cycles retain their stable existing order.
pub fn raise_family(stack: &mut Vec<u64>, selected: u64, parents: &HashMap<u64, u64>) {
    let mut seen = HashSet::new();
    stack.retain(|id| seen.insert(*id));
    let path = ancestors(selected, parents);
    let Some(root) = path.last().copied() else {
        return;
    };
    let path: HashSet<_> = path.into_iter().collect();
    let family = inherit_flags(parents, &HashSet::from([root]), stack.iter().copied());
    let members: Vec<_> = stack
        .iter()
        .copied()
        .filter(|id| family.contains(id))
        .collect();
    let ranks: HashMap<_, _> = members
        .iter()
        .enumerate()
        .map(|(rank, id)| (*id, rank))
        .collect();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut pending = HashSet::new();
    for id in &members {
        if let Some(parent) = parents.get(id).filter(|parent| family.contains(parent)) {
            children.entry(*parent).or_default().push(*id);
            pending.insert(*id);
        }
    }
    let priority = |id: u64| Reverse((path.contains(&id), ranks[&id], id));
    let mut ready = BinaryHeap::new();
    for id in &members {
        if !pending.contains(id) {
            ready.push(priority(*id));
        }
    }
    let mut ordered = Vec::with_capacity(members.len());
    let mut emitted = HashSet::new();
    while let Some(Reverse((_, _, id))) = ready.pop() {
        if !emitted.insert(id) {
            continue;
        }
        ordered.push(id);
        if let Some(kids) = children.get(&id) {
            for child in kids {
                ready.push(priority(*child));
            }
        }
    }
    ordered.extend(members.into_iter().filter(|id| !emitted.contains(id)));
    stack.retain(|id| !family.contains(id));
    stack.extend(ordered);
}
#[cfg(test)]
mod tests {
    use super::*;
    fn chain() -> HashMap<u64, u64> {
        [(2, 1), (3, 2), (4, 1)].into()
    }
    #[test]
    fn parent_hides_children_without_changing_own_flags() {
        let own = HashSet::from([1]);
        assert_eq!(
            inherit_flags(&chain(), &own, 1..=5),
            HashSet::from([1, 2, 3, 4])
        );
        assert_eq!(own, HashSet::from([1]));
    }
    #[test]
    fn independently_minimized_child_stays_hidden_after_parent_restore() {
        assert_eq!(
            inherit_flags(&chain(), &HashSet::from([2]), 1..=5),
            HashSet::from([2, 3])
        );
    }
    #[test]
    fn cycle_without_minimized_member_is_visible() {
        assert!(inherit_flags(
            &HashMap::from([(1, 2), (2, 1), (3, 2)]),
            &HashSet::new(),
            1..=3
        )
        .is_empty());
    }
    #[test]
    fn cycle_with_minimized_member_hides_all_members_and_descendants() {
        assert_eq!(
            inherit_flags(
                &HashMap::from([(1, 2), (2, 1), (3, 2)]),
                &HashSet::from([1]),
                1..=3
            ),
            HashSet::from([1, 2, 3])
        );
    }
    #[test]
    fn long_chain_does_not_recurse() {
        let parents = (2..=10000).map(|id| (id, id - 1)).collect();
        assert_eq!(
            inherit_flags(&parents, &HashSet::from([1]), 1..=10000).len(),
            10000
        );
    }
    #[test]
    fn activating_child_restores_ancestors() {
        let mut own = HashSet::from([1, 2, 3, 4]);
        for id in ancestors(3, &chain()) {
            own.remove(&id);
        }
        assert_eq!(own, HashSet::from([4]));
    }
    #[test]
    fn activating_parent_preserves_independent_child_minimize() {
        let mut own = HashSet::from([1, 2]);
        for id in ancestors(1, &chain()) {
            own.remove(&id);
        }
        assert_eq!(own, HashSet::from([2]));
    }
    #[test]
    fn ancestor_restore_is_cycle_safe() {
        assert_eq!(ancestors(1, &HashMap::from([(1, 2), (2, 1)])), vec![1, 2]);
    }
    #[test]
    fn raising_selected_modal_raises_root_and_keeps_siblings_below_selected() {
        let mut stack = vec![1, 2, 3, 4, 5];
        raise_family(&mut stack, 3, &HashMap::from([(2, 1), (3, 1)]));
        assert_eq!(stack, vec![4, 5, 1, 2, 3]);
    }
    #[test]
    fn raising_parent_keeps_all_transients_above_it() {
        let mut stack = vec![2, 3, 1, 4];
        raise_family(&mut stack, 1, &HashMap::from([(2, 1), (3, 2)]));
        assert_eq!(stack, vec![4, 1, 2, 3]);
    }
    #[test]
    fn nested_selection_orders_unrelated_siblings_first() {
        let mut stack = vec![1, 2, 3, 4, 5, 6];
        raise_family(
            &mut stack,
            3,
            &HashMap::from([(2, 1), (3, 2), (4, 1), (5, 2)]),
        );
        assert_eq!(stack, vec![6, 1, 4, 2, 5, 3]);
    }
    #[test]
    fn raising_cycle_is_bounded_and_preserves_stable_order() {
        let mut stack = vec![1, 2, 3, 4];
        raise_family(&mut stack, 1, &HashMap::from([(1, 2), (2, 1), (3, 2)]));
        assert_eq!(stack, vec![4, 1, 2, 3]);
    }
    #[test]
    fn raising_deduplicates_old_stack_entries() {
        let mut stack = vec![1, 2, 2, 3];
        raise_family(&mut stack, 2, &HashMap::from([(2, 1)]));
        assert_eq!(stack, vec![3, 1, 2]);
    }
}
