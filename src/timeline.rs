//! A subproject's timeline (inspector → Timeline). Filled in below.

use gpui::{AnyElement, Context, IntoElement, div};

use crate::Monaged;

impl Monaged {
    pub(crate) fn render_timeline(&self, _sub_id: i64, _cx: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }
}
