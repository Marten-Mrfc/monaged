//! Git in the inspector (history, changes, sync, conflicts). Filled in below.

use gpui::{AnyElement, Context, IntoElement, div};

use crate::Monaged;

impl Monaged {
    pub(crate) fn render_git_inspector(&self, _sub_id: i64, _cx: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }
}
