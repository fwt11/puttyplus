//! 图标装载冒烟（4.2）：内嵌 PNG → 落盘 → 挂图标主题 → GTK 真能解码出纹理。
//! 运行：cargo run --example icon_smoke
use gtk::prelude::*;
use puttyplus::appicon;

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }
    let name = appicon::install().expect("应能装进图标主题");
    let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
    println!("图标名 = {name}");
    println!("主题里有 = {}", theme.has_icon(name));
    if let Some(t) = appicon::texture() {
        println!("about logo 纹理 = {}×{}", t.width(), t.height());
    }
    assert!(theme.has_icon(name), "图标主题里找不到 {name}");
}
