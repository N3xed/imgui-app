use imgui_app::*;

struct HelloWorldWindow {}

impl imgui_app::Layout for HelloWorldWindow {
    fn layout(
        &mut self,
        ui: LayoutContext<'_>,
        _: &App,
        _: &mut Window,
    ) {
        let mut opened = true;
        ui.show_demo_window(&mut opened);
    }
    fn log(&mut self, _: Option<log::Level>, _: &str) {}
    fn as_any(&mut self) -> &mut (dyn std::any::Any + 'static) {
        self
    }
    fn init(
        &mut self,
        _: &mut Window,
    ) -> Result<(), Box<(dyn std::error::Error + 'static)>> {
        println!("init window");
        Ok(())
    }
    fn before_close(&mut self, _: &mut Window) -> bool {
        println!("before close");
        true
    }
}

fn main() {
    let mut app = App::new();

    app.new_window(HelloWorldWindow {}, &|app| {
        Ok(Window::build_window("Hello World", (1280, 720)))
    })
    .unwrap();

    app.run();
}
