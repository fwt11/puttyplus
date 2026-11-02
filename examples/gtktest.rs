//! 最小复现：**GTK 4.18 的 `gtk_window_close()` 只有第一次有效**。
//!
//! 现象：窗口第一次 `close()` 会藏起来；之后再 `close()` **只发
//! `close-request` 信号、窗口不藏** ⇒ 用户看到「第一次能关，第二次关不掉」。
//!
//! ⚠️ **不是我们代码的锅**——这个例子用的是**一个空窗口**，什么回调都没有。
//! ⇒ 我们的工具窗一律用 `set_visible(false)` 藏（见
//! `src/panel/dbui.rs::hide_tool_window`）。
//!
//! 用法：xvfb-run -a cargo run --example gtktest

use gtk::prelude::*;

fn main() {
    let app = gtk::Application::builder().application_id("x.y.PpGtkTest").build();
    app.connect_activate(|_| {
        let w = gtk::Window::builder()
            .title("t")
            .default_width(300)
            .default_height(200)
            .build();
        w.present();
        println!("1 开：visible={}", w.is_visible());
        w.close();
        println!("1 关：visible={} realized={}", w.is_visible(), w.is_realized());
        w.present();
        println!("2 开：visible={} realized={}", w.is_visible(), w.is_realized());
        w.close();
        println!("2 关：visible={}　← 关不掉了！", w.is_visible());
        w.set_visible(false);
        println!("用 set_visible(false)：visible={}　← 这个才行", w.is_visible());
    });
    app.run_with_args::<&str>(&[]);
}
