//! Port of `utils.c`, `internal/utils.h`: clamping, semitone wrapping, hook matching and the intrusive lists.
//!
//! Upstream threads doubly-linked lists through `prev`/`next` pointers at the start of pooled structs. Here each
//! pooled node carries [`Links`] holding pool indices, and the list functions take the pool and its head. The
//! semantics are the same, including the corner cases of unlinking a node while a walker holds its successor.

/// The `prev`/`next` links of a pooled node, as indices into its pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Links {
    pub prev: Option<u8>,
    pub next: Option<u8>,
}

/// A pooled node that can sit on an intrusive list.
pub(crate) trait Linked {
    fn links(&self) -> &Links;
    fn links_mut(&mut self) -> &mut Links;
}

/// The head of an intrusive list over a pool.
pub(crate) type ListHead = Option<u8>;

#[doc(alias = "ImUtils_Clamp")]
pub(crate) fn clamp(v: i32, min: i32, max: i32) -> i32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// Adds or subtracts 12 until `v` lies within `[min, max]`.
#[doc(alias = "ImUtils_WrapSemitones")]
pub(crate) fn wrap_semitones(mut v: i32, min: i32, max: i32) -> i32 {
    while v < min {
        v += 12;
    }
    while v > max {
        v -= 12;
    }
    v
}

/// Head insertion. A node whose links are not both clear is rejected, as upstream does.
#[doc(alias = "ImUtils_ListAddItem")]
pub(crate) fn list_add_item<T: Linked>(pool: &mut [T], head: &mut ListHead, item: u8) {
    let links = *pool[usize::from(item)].links();
    if links.prev.is_some() || links.next.is_some() {
        tracing::info!(target: "imuse", "ERR: list arg err on ListAddItem...");
        return;
    }
    let old = *head;
    pool[usize::from(item)].links_mut().next = old;
    if let Some(old) = old {
        pool[usize::from(old)].links_mut().prev = Some(item);
    }
    pool[usize::from(item)].links_mut().prev = None;
    *head = Some(item);
}

/// Unlinks `item`. Returns 0, -3 when the item is not on the list, or -5 when the list is empty.
#[doc(alias = "ImUtils_ListRemoveItem")]
pub(crate) fn list_remove_item<T: Linked>(pool: &mut [T], head: &mut ListHead, item: u8) -> i32 {
    let Some(mut cur) = *head else {
        tracing::info!(target: "imuse", "ERR: list arg err on ListRemoveItem...");
        return -5;
    };
    while cur != item {
        match pool[usize::from(cur)].links().next {
            Some(next) => cur = next,
            None => {
                tracing::info!(target: "imuse", "ERR: item not on list...");
                return -3;
            }
        }
    }
    let Links { prev, next } = *pool[usize::from(item)].links();
    if let Some(next) = next {
        pool[usize::from(next)].links_mut().prev = prev;
    }
    match prev {
        Some(prev) => pool[usize::from(prev)].links_mut().next = next,
        None => *head = next,
    }
    *pool[usize::from(item)].links_mut() = Links::default();
    0
}

/// Evaluates a SysEx jump hook against the player's armed hook. Returns 0 (fire) or -1 (block).
///
/// A non-zero `track_hook_id` fires only when it equals the armed hook, consuming it. A wildcard (0) fires unless
/// the armed hook is 128, which suppresses wildcards once and is consumed.
#[doc(alias = "ImUtils_CheckHookValue")]
pub(crate) fn check_hook_value(player_hook: &mut i32, track_hook_id: i32) -> i32 {
    if track_hook_id != 0 {
        if track_hook_id == *player_hook {
            *player_hook = 0;
            return 0;
        }
        return -1;
    }
    if *player_hook == 128 {
        *player_hook = 0;
        return -1;
    }
    0
}

/// Reads a big-endian `u32`.
#[doc(alias = "ImUtils_Swap32")]
pub(crate) fn swap32(buf: &[u8]) -> u32 {
    u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Node {
        links: Links,
    }

    impl Linked for Node {
        fn links(&self) -> &Links {
            &self.links
        }
        fn links_mut(&mut self) -> &mut Links {
            &mut self.links
        }
    }

    fn order(pool: &[Node], head: ListHead) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = head;
        while let Some(index) = cur {
            out.push(index);
            cur = pool[usize::from(index)].links.next;
        }
        out
    }

    #[test]
    fn lists_insert_at_head_and_unlink() {
        let mut pool: Vec<Node> = (0..4).map(|_| Node::default()).collect();
        let mut head = None;
        for i in 0..4 {
            list_add_item(&mut pool, &mut head, i);
        }
        assert_eq!(order(&pool, head), [3, 2, 1, 0]);
        assert_eq!(list_remove_item(&mut pool, &mut head, 2), 0);
        assert_eq!(order(&pool, head), [3, 1, 0]);
        assert_eq!(list_remove_item(&mut pool, &mut head, 3), 0);
        assert_eq!(order(&pool, head), [1, 0]);
        assert_eq!(list_remove_item(&mut pool, &mut head, 3), -3);
        assert_eq!(pool[3].links, Links::default());
        // A linked non-head node is rejected.
        list_add_item(&mut pool, &mut head, 0);
        assert_eq!(order(&pool, head), [1, 0]);
        let mut empty = None;
        assert_eq!(list_remove_item(&mut pool, &mut empty, 0), -5);
    }

    #[test]
    fn helpers_match_upstream() {
        assert_eq!(clamp(5, 0, 3), 3);
        assert_eq!(wrap_semitones(15, -12, 12), 3);
        assert_eq!(wrap_semitones(-25, -12, 12), -1);
        let mut hook = 5;
        assert_eq!(check_hook_value(&mut hook, 4), -1);
        assert_eq!(check_hook_value(&mut hook, 5), 0);
        assert_eq!(hook, 0);
        hook = 128;
        assert_eq!(check_hook_value(&mut hook, 0), -1);
        assert_eq!((hook, check_hook_value(&mut hook, 0)), (0, 0));
        assert_eq!(swap32(&[1, 2, 3, 4]), 0x0102_0304);
    }
}
