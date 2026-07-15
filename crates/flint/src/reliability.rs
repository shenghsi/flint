use gpui::App;

mod hang_detection;

pub fn init(cx: &mut App) {
    hang_detection::start(cx);
}
