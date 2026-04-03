/// Adjust `scroll_offset` so that `selected` is visible within `visible_height`.
/// Only scrolls when the cursor would otherwise be outside the visible window.
pub fn ensure_visible(selected: usize, scroll_offset: &mut usize, visible_height: usize) {
    if visible_height == 0 {
        return;
    }
    if selected < *scroll_offset {
        *scroll_offset = selected;
    } else if selected >= *scroll_offset + visible_height {
        *scroll_offset = selected + 1 - visible_height;
    }
}

/// Scroll viewport by `delta` rows, clamping to valid range.
/// Does NOT change selection.
pub fn scroll_viewport(
    scroll_offset: &mut usize,
    delta: isize,
    list_len: usize,
    visible_height: usize,
) {
    if list_len <= visible_height {
        *scroll_offset = 0;
        return;
    }
    let max_offset = list_len.saturating_sub(visible_height);
    if delta < 0 {
        *scroll_offset = scroll_offset.saturating_sub(delta.unsigned_abs());
    } else {
        *scroll_offset = (*scroll_offset + delta as usize).min(max_offset);
    }
}
