//! Drag-and-drop reordering: what is being dragged (a tab, a sidebar connection),
//! its floating preview, and the drop marker on the target.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::*;

/// A tab being dragged in the tab bar.
#[derive(Clone)]
pub struct DraggedTab {
    pub ix: usize,
    /// Where in the tab the drag started (set when the drag begins).
    pub grab: Point<Pixels>,
    pub icon: Lucide,
    pub title: SharedString,
}

/// A saved connection being dragged in the sidebar.
#[derive(Clone)]
pub struct DraggedConnection {
    pub id: String,
    /// Position in the saved connections list.
    pub ix: usize,
    pub icon: Lucide,
    pub name: SharedString,
    /// Where in the row the drag started (set when the drag begins).
    pub grab: Point<Pixels>,
}

/// The floating chip under the pointer. gpui places the preview where the item was
/// grabbed, which would cover the drop target; shift it to just below-right of the
/// pointer instead.
fn preview(icon: Lucide, label: SharedString, grab: Point<Pixels>, cx: &App) -> Div {
    let theme = cx.theme();
    let chip = h_flex()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded(theme.radius)
        .bg(theme.popover)
        .border_1()
        .border_color(theme.border)
        .shadow_md()
        // See-through, so the drop marker under it stays visible.
        .opacity(0.85)
        .text_sm()
        .text_color(theme.popover_foreground)
        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
        .child(label);
    div().pl(grab.x + px(12.)).pt(grab.y + px(16.)).child(chip)
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        preview(self.icon, self.title.clone(), self.grab, cx)
    }
}

impl Render for DraggedConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        preview(self.icon, self.name.clone(), self.grab, cx)
    }
}

/// Style for the drop target at `target` while the item at `from` is dragged over it:
/// a bar on the side where the item will land (after the target when it moves forward,
/// before it when it moves back). `horizontal` = tabs, otherwise a vertical list.
pub fn drop_marker(style: StyleRefinement, from: usize, target: usize, horizontal: bool, cx: &App) -> StyleRefinement {
    if from == target {
        return style;
    }
    let color = cx.theme().primary;
    let forward = from < target;
    match (horizontal, forward) {
        (true, true) => style.border_r_2().border_color(color),
        (true, false) => style.border_l_2().border_color(color),
        (false, true) => style.border_b_2().border_color(color),
        (false, false) => style.border_t_2().border_color(color),
    }
}
