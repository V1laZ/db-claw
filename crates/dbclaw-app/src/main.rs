use gpui_kit::component::button::*;
use gpui_kit::component::*;
use gpui_kit::*;

pub struct Hello;
impl Render for Hello {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().v_flex().size_full().child(Button::new("ok").primary().label("Hi"))
    }
}

fn main() {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Hello)).unwrap();
    });
}
