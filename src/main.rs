slint::include_modules!();

fn main() {
    let app = AppWindow::new().unwrap();
    let weak = app.as_weak();
    app.on_submit(move |_text| {
        if let Some(app) = weak.upgrade() {
            app.set_prompt("".into());
        }
    });
    app.run().unwrap();
}
