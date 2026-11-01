//! 会话回放窗口（6.1）。
//!
//! 打开一个 `.cast`（asciicast v2）按原始时序喂给一个**没有子进程**的
//! VTE 终端。控件：重播 / 播放暂停 / 倍速 / 进度，状态条显示当时敲了
//! 什么键。
//!
//! 明确不做的一件事：**不把录到的输入真发出去**。回放终端压根没挂 PTY
//! （往一个真会话重放按键更危险），输入只显示出来供复盘。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use gtk::gio;
use gtk::prelude::*;
use vte4::prelude::*;

use crate::replay::{self, Cast, EventKind};

/// 倍速档位。
const SPEEDS: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];

/// 一次空转最多挂多久（毫秒）。事件之间有大空档时用不着醒着，
/// 但也不能睡太久，否则进度条看着是卡的。
const IDLE_MS: u64 = 250;

pub fn set_parent(w: gtk::Window) {
    // ⚠️ 父窗口统一由 `windows` 管（阶段 67）
    crate::windows::set_parent(w);
}

fn parent() -> Option<gtk::Window> {
    crate::windows::parent_of(None)
}

/* ==================================================================
 * 入口
 * ================================================================== */

/// 弹文件选择器挑一个 `.cast` 回放。
pub fn pick_and_open() {
    let parent = parent();
    // builder 的 transient_for 收 &W 不收 Option，没父窗口就别设
    let mut b = gtk::FileChooserNative::builder()
        .title("打开会话录制")
        .action(gtk::FileChooserAction::Open)
        .accept_label("回放")
        .cancel_label("取消")
        .modal(true);
    if let Some(p) = parent.as_ref() {
        b = b.transient_for(p);
    }
    let dlg = b.build();
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("会话录制（asciicast v2 / v3）"));
    filter.add_pattern("*.cast");
    filter.add_mime_type("application/x-asciicast");
    dlg.add_filter(&filter);
    if let Some(dir) = latest_dir() {
        let _ = dlg.set_current_folder(Some(&gio::File::for_path(dir)));
    }
    dlg.connect_response(move |d, r| {
        if r == gtk::ResponseType::Accept {
            if let Some(f) = d.file() {
                if let Some(p) = f.path() {
                    if let Err(e) = open(&p) {
                        crate::term::alert::<gtk::Window>(parent.as_ref(), &e);
                    }
                }
            }
        }
        d.destroy();
    });
    dlg.show();
}

/// 最近一次录制所在的目录（没有就回落到用户数据目录）。
fn latest_dir() -> Option<PathBuf> {
    let d = crate::term::default_capture_dir();
    d.is_dir().then_some(d)
}

/// 回放最近一次录制（录制目录下时间最新的那个 `.cast`）。
pub fn replay_latest() -> Result<(), String> {
    let p = latest()
        .ok_or_else(|| format!("{} 下还没有录制文件。", crate::term::default_capture_dir().display()))?;
    open(&p)
}

fn latest() -> Option<PathBuf> {
    let dir = crate::term::default_capture_dir();
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()? {
        let Ok(e) = e else { continue };
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("cast") {
            continue;
        }
        let Ok(t) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

/// 打开一个录制文件回放。
pub fn open(path: &Path) -> Result<(), String> {
    let cast = replay::load(path)?;
    Ok(build(path, cast))
}

/* ==================================================================
 * 播放引擎
 * ================================================================== */

struct Player {
    cast: Cast,
    term: vte4::Terminal,
    /// 下一条要处理的事件下标
    idx: Cell<usize>,
    playing: Cell<bool>,
    speed: Cell<f64>,
    /// 播放到的时间点（秒）
    pos: Cell<f64>,
    /// 推算 pos 的墙上时间基准
    wall: Cell<Instant>,
    src: RefCell<Option<glib::SourceId>>,
    play_btn: gtk::Button,
    pos_lbl: gtk::Label,
    input_lbl: gtk::Label,
    bar: gtk::ProgressBar,
}

impl Player {
    fn total(&self) -> f64 {
        self.cast.duration.max(0.0)
    }

    fn sync_play_btn(&self) {
        self.play_btn.set_label(if self.playing.get() {
            "⏸ 暂停"
        } else {
            "▶ 播放"
        });
    }

    fn refresh(&self) {
        let pos = self.pos.get();
        let total = self.total();
        self.pos_lbl.set_text(&format!(
            "{} / {}",
            replay::fmt_time(pos.min(total)),
            replay::fmt_time(total)
        ));
        self.bar.set_fraction(if total > 0.0 {
            (pos / total).clamp(0.0, 1.0)
        } else {
            1.0
        });
    }

    /// 把所有到点的事件喂出去。
    fn drain(&self) {
        let pos = self.pos.get();
        loop {
            let i = self.idx.get();
            if i >= self.cast.events.len() || self.cast.events[i].t > pos {
                break;
            }
            let ev = &self.cast.events[i];
            match ev.kind {
                EventKind::Output => self.term.feed(ev.data.as_bytes()),
                EventKind::Resize => {
                    if let Some((c, r)) = parse_size(&ev.data) {
                        self.term.set_size(c, r);
                    }
                }
                // 录到的按键只显示，不发（见模块头说明）
                EventKind::Input => {
                    let d = replay::input_display(&ev.data);
                    self.input_lbl.set_text(&format!("⌨ {d}"));
                    self.input_lbl.set_tooltip_text(Some(&d));
                }
                EventKind::Marker => {
                    self.input_lbl.set_text(&format!("§ {}", ev.data));
                    self.input_lbl.set_tooltip_text(Some(&ev.data));
                }
                // v3 的退出事件：会话是正常退出还是出错退出，看得见
                EventKind::Exit => {
                    let code = ev.data.trim();
                    let txt = if code == "0" {
                        "会话已正常退出".to_string()
                    } else {
                        format!("会话已退出（退出码 {code}）")
                    };
                    self.input_lbl.set_text(&txt);
                    self.input_lbl.set_tooltip_text(Some(&txt));
                }
            }
            self.idx.set(i + 1);
        }
    }

    fn stop_src(&self) {
        if let Some(s) = self.src.borrow_mut().take() {
            s.remove();
        }
    }

    /// 重新挂一次定时器，延时取到下一条事件的剩余时间。
    fn arm(self: &Rc<Self>) {
        self.stop_src();
        if !self.playing.get() {
            return;
        }
        let next = self
            .cast
            .events
            .get(self.idx.get())
            .map(|e| e.t)
            .unwrap_or(f64::INFINITY);
        let left = next - self.pos.get();
        if !left.is_finite() {
            // 放完了
            self.playing.set(false);
            self.sync_play_btn();
            self.refresh();
            return;
        }
        let ms: u64 = (((left.max(0.0) * 1000.0) / self.speed.get().max(0.01))
            .min(IDLE_MS as f64)
            .ceil() as u64)
            .max(1);
        let me = Rc::clone(self);
        let id =
            glib::timeout_add_local(std::time::Duration::from_millis(ms), move || me.tick());
        *self.src.borrow_mut() = Some(id);
    }

    fn tick(self: &Rc<Self>) -> glib::ControlFlow {
        let speed = self.speed.get();
        let pos = self.pos.get() + self.wall.get().elapsed().as_secs_f64() * speed;
        self.wall.set(Instant::now());
        self.pos.set(pos);
        self.drain();
        self.refresh();
        if self.idx.get() >= self.cast.events.len() {
            self.playing.set(false);
            self.sync_play_btn();
            self.stop_src();
            return glib::ControlFlow::Break;
        }
        self.arm();
        glib::ControlFlow::Continue
    }

    fn play(self: &Rc<Self>) {
        if self.idx.get() >= self.cast.events.len() {
            self.restart();
        }
        if self.playing.get() {
            return;
        }
        self.playing.set(true);
        self.wall.set(Instant::now());
        self.sync_play_btn();
        self.arm();
    }

    fn pause(self: &Rc<Self>) {
        self.playing.set(false);
        self.stop_src();
        self.sync_play_btn();
    }

    fn toggle(self: &Rc<Self>) {
        if self.playing.get() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// 从头重放：清屏、游标归位、索引归零。
    fn restart(self: &Rc<Self>) {
        self.pause();
        self.idx.set(0);
        self.pos.set(0.0);
        self.wall.set(Instant::now());
        self.term.reset(true, true);
        self.term.feed(b"\x1b[2J\x1b[H");
        self.input_lbl.set_text("");
        self.refresh();
        self.play();
    }

    /// 跳到下一个"有内容"的点（输出 / 输入 / 标记）。
    ///
    /// ⚠ **输入事件我们不录**（asciicast v2 规范：不应默认录输入，
    /// 录了就有明文密码），所以"跳过等待"要靠**输出**事件来找——
    /// 用户发呆 30 秒时中间一个事件都没有，跳到下一个输出正好跳过那段等待。
    fn skip_to_next_interesting(self: &Rc<Self>) {
        let i = self.idx.get();
        let mut j = i;
        while j < self.cast.events.len()
            && !matches!(
                self.cast.events[j].kind,
                EventKind::Input | EventKind::Marker | EventKind::Output
            )
        {
            j += 1;
        }
        if j >= self.cast.events.len() {
            self.restart();
            return;
        }
        // 跳过去之前先把中间的输出都补上（不然画面会缺一段）
        self.pos.set(self.cast.events[j].t);
        self.drain();
        self.refresh();
        self.wall.set(Instant::now());
        if !self.playing.get() {
            self.play();
        } else {
            self.arm();
        }
    }
}

/// `"120x40"` → `(120, 40)`。解析不出来给 None（不猜尺寸）。
fn parse_size(s: &str) -> Option<(i64, i64)> {
    let (c, r) = s.split_once(['x', 'X'])?;
    let c: i64 = c.trim().parse().ok()?;
    let r: i64 = r.trim().parse().ok()?;
    if c <= 0 || r <= 0 {
        return None;
    }
    Some((c.min(1000), r.min(1000)))
}

/* ==================================================================
 * 窗口
 * ================================================================== */

fn build(path: &Path, cast: Cast) {
    let (rw, rh) = crate::settings::window_size("replay").unwrap_or((900, 600));
    let win = gtk::ApplicationWindow::builder()
        .title(format!(
            "回放 {} — PuTTYPlus",
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "录制".into())
        ))
        .default_width(rw)
        .default_height(rh)
        .build();
    if let Some(p) = parent() {
        win.set_transient_for(Some(&p));
    }

    let term = vte4::Terminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    term.set_scrollback_lines(0);
    crate::term::apply_colors(&term);
    crate::term::set_appearance(&term, None, None);
    term.set_size(cast.cols, cast.rows);

    let play_btn = gtk::Button::with_label("▶ 播放");
    let pos_lbl = gtk::Label::new(None);
    pos_lbl.add_css_class("monospace");
    let input_lbl = gtk::Label::new(None);
    input_lbl.set_xalign(0.0);
    input_lbl.set_hexpand(true);
    input_lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    input_lbl.add_css_class("dim-label");

    let bar = gtk::ProgressBar::new();
    bar.set_show_text(false);

    let speed_labels: Vec<String> = SPEEDS.iter().map(|s| speed_label(*s)).collect();
    let speed_dd =
        gtk::DropDown::from_strings(&speed_labels.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    speed_dd.set_selected(2); // 默认 1×
    speed_dd.set_tooltip_text(Some("回放速度"));

    let player = Rc::new(Player {
        cast,
        term: term.clone(),
        idx: Cell::new(0),
        playing: Cell::new(false),
        speed: Cell::new(1.0),
        pos: Cell::new(0.0),
        wall: Cell::new(Instant::now()),
        src: RefCell::new(None),
        play_btn: play_btn.clone(),
        pos_lbl: pos_lbl.clone(),
        input_lbl: input_lbl.clone(),
        bar: bar.clone(),
    });

    /* ---- 控制条 ---- */
    let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar_box.set_margin_top(6);
    bar_box.set_margin_bottom(4);
    bar_box.set_margin_start(8);
    bar_box.set_margin_end(8);
    let b_restart = gtk::Button::with_label("⏮ 重播");
    b_restart.set_tooltip_text(Some("从头播放"));
    let b_skip = gtk::Button::with_label("⏭ 下一处输出");
    b_skip.set_tooltip_text(Some("跳到下一次输出或标记（复盘时跳过等待）"));
    bar_box.append(&b_restart);
    bar_box.append(&play_btn);
    bar_box.append(&b_skip);
    bar_box.append(&gtk::Label::new(Some("速度")));
    bar_box.append(&speed_dd);
    bar_box.append(&pos_lbl);

    {
        let p = Rc::clone(&player);
        play_btn.connect_clicked(move |_| p.toggle());
    }
    {
        let p = Rc::clone(&player);
        b_restart.connect_clicked(move |_| p.restart());
    }
    {
        let p = Rc::clone(&player);
        b_skip.connect_clicked(move |_| p.skip_to_next_interesting());
    }
    {
        let p = Rc::clone(&player);
        speed_dd.connect_selected_notify(move |dd| {
            let i = dd.selected() as usize;
            let s = SPEEDS.get(i).copied().unwrap_or(1.0);
            p.speed.set(s);
            if p.playing.get() {
                p.wall.set(Instant::now());
                p.arm();
            }
        });
    }

    /* ---- 布局 ---- */
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&bar_box);
    root.append(&bar);
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    root.append(&sep);
    root.append(&term);
    root.append(&sep);
    root.append(&input_lbl);

    let tip = gtk::Label::new(Some(
        "回放只是把录到的内容重放一遍：颜色是按屏幕还原的；\
         输出快过采样间隔、滚动缓冲被挤爆时会缺；输入事件不录（文件里不会有你敲的密码）。",
    ));
    tip.add_css_class("dim-label");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    tip.set_margin_top(4);
    tip.set_margin_bottom(6);
    tip.set_margin_start(8);
    tip.set_margin_end(8);
    root.append(&tip);

    win.set_child(Some(&root));

    // 空格播放/暂停、右键→左键跳过、Ctrl+R 重播
    {
        let p = Rc::clone(&player);
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, kv, _, m| {
            let ctrl = m.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let name = kv.name().unwrap_or_default();
            match (ctrl, name.as_str()) {
                (false, "space") => p.toggle(),
                (false, "Right") => p.skip_to_next_interesting(),
                (true, "r") => p.restart(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        win.add_controller(key);
    }

    // 关窗必须摘定时器：它抓着 Rc<Player>，不摘就是一条引用环，
    // 而且这个源会一直每 IDLE_MS 醒一次
    {
        let p = Rc::clone(&player);
        win.connect_close_request(move |w| {
            crate::settings::save_window_size("replay", w);
            p.stop_src();
            glib::Propagation::Proceed
        });
    }

    player.refresh();
    win.present();
    // 摆好窗口就自动开始，省得再点一下
    player.play();
}

fn speed_label(s: f64) -> String {
    if s >= 1.0 {
        format!("{}×", s as i64)
    } else {
        format!("{}×", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 尺寸解析() {
        assert_eq!(parse_size("120x40"), Some((120, 40)));
        assert_eq!(parse_size(" 80 x 24 "), Some((80, 24)));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("0x0"), None);
        assert_eq!(parse_size("-1x5"), None);
    }

    #[test]
    fn 尺寸解析有上限() {
        // 录里写个 999999 行也不能真给终端设那么大
        assert_eq!(parse_size("99999x99999"), Some((1000, 1000)));
    }

    #[test]
    fn 倍速显示() {
        assert_eq!(speed_label(1.0), "1×");
        assert_eq!(speed_label(4.0), "4×");
        assert_eq!(speed_label(0.5), "0.5×");
    }
}
