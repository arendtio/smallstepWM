use std::{
    process::Command,
    time::{Duration, Instant},
};

use smithay::{
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        input::ScrollMethod,
    },
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, ButtonState,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    handlers::surface_at,
    pages::{self, Command as PageCommand},
    shell::{self, Entry, Page, PowerAction, WifiHit},
    state::State,
    voice::{self, Report},
    wifi::{self, PasswordEdit, WifiJob},
};

enum WifiJobKind {
    Refresh,
    Radio(bool),
    Connect { ssid: String, password: Option<String> },
    Disconnect,
}

impl State {
    pub fn pointer_moved(&mut self, pos: Point<f64, Logical>) {
        let size = self.output_size;
        let pos = Point::from((
            pos.x.clamp(0.0, (size.w.saturating_sub(1).max(0)) as f64),
            pos.y.clamp(0.0, (size.h.saturating_sub(1).max(0)) as f64),
        ));
        let over_strip = pos.x < f64::from(shell::strip_width(self.strip_expanded));
        let expanded = self.power_menu || self.wifi.open || over_strip;
        self.pointer = pos;
        self.strip_expanded = expanded;
        self.point_pointer();
        self.mark_dirty();
    }

    pub fn on_input<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time() as u32;
                let pressed = event.state() == KeyState::Pressed;
                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |state, _, keysym| {
                        let sym = keysym.modified_sym();
                        if sym.raw() == smithay::input::keyboard::keysyms::KEY_F1 {
                            if voice::f1_edge(&mut state.voice_f1_down, pressed) {
                                state.toggle_voice();
                            }
                            return FilterResult::Intercept(());
                        }
                        if state.wifi.open && state.wifi.password_for.is_some() {
                            if pressed {
                                let text = smithay::input::keyboard::xkb::keysym_to_utf8(sym);
                                state.edit_password(sym.raw(), &text);
                            }
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
            }
            InputEvent::PointerMotion { event } => {
                let delta = event.delta();
                self.pointer_moved(self.pointer + delta);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let pos = event.position_transformed(self.output_size);
                self.pointer_moved(pos);
            }
            InputEvent::PointerButton { event } => self.pointer_button(
                event.button_code(),
                event.state(),
                event.time() as u32,
            ),
            InputEvent::PointerAxis { event } => {
                if self.wifi.open && shell::wifi_panel_contains(self.output_size.h, self.pointer.x, self.pointer.y) {
                    if let Some(amount) = event.amount(Axis::Vertical) {
                        self.scroll_wifi(amount);
                    }
                    return;
                }
                if self.strip_grab || self.strip_hit().is_some() {
                    if let Some(amount) = event.amount(Axis::Vertical) {
                        self.scroll_pages(amount);
                    }
                    return;
                }
                self.point_pointer();
                let source = event.source();
                let contact = matches!(source, AxisSource::Finger | AxisSource::Continuous);
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    frame = frame.relative_direction(axis, event.relative_direction(axis));
                    match scroll_step(contact, event.amount(axis)) {
                        ScrollStep::Move(amount) => frame = frame.value(axis, amount),
                        ScrollStep::Stop => frame = frame.stop(axis),
                        ScrollStep::Skip => {}
                    }
                    if let Some(discrete) = event.amount_v120(axis) {
                        let steps = discrete as i32;
                        if steps != 0 {
                            frame = frame.v120(axis, steps);
                        }
                    }
                }
                self.seat.get_pointer().unwrap().axis(self, frame);
                self.seat.get_pointer().unwrap().frame(self);
            }
            _ => {}
        }
    }

    fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let serial = SERIAL_COUNTER.next_serial();
        let on_strip = self.strip_hit();
        if state == ButtonState::Pressed {
            if let Some(entry) = on_strip {
                self.strip_grab = true;
                self.activate(entry);
                return;
            }
            if self.wifi.open {
                if let Some(hit) = shell::hit_wifi(
                    self.output_size.h,
                    self.pointer.x,
                    self.pointer.y,
                    self.wifi.networks.len(),
                    self.wifi.scroll,
                ) {
                    self.strip_grab = true;
                    self.wifi_hit(hit);
                    return;
                }
                self.close_wifi();
            }
            if self.power_menu {
                if let Some(action) = shell::hit_power_action(self.output_size.h, self.pointer.x, self.pointer.y) {
                    self.strip_grab = true;
                    self.run_power(action);
                    return;
                }
                self.power_menu = false;
                self.strip_expanded = self.pointer.x < f64::from(shell::STRIP_COLLAPSED);
                self.mark_dirty();
            }
            self.strip_grab = false;
            if self.page == Page::Browser {
                if let Some((window, _)) = self.space.element_under(self.pointer) {
                    let window = window.clone();
                    self.space.raise_element(&window, true);
                    if let Some(top) = window.toplevel() {
                        self.seat.get_keyboard().unwrap().set_focus(
                            self,
                            Some(top.wl_surface().clone()),
                            serial,
                        );
                    }
                }
            }
        } else if self.strip_grab {
            self.strip_grab = false;
            return;
        }

        self.point_pointer();
        let pointer = self.seat.get_pointer().unwrap();
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    fn point_pointer(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        let h = self.output_size.h;
        let over_chrome = self.strip_hit().is_some()
            || (self.wifi.open && shell::wifi_panel_contains(h, self.pointer.x, self.pointer.y))
            || (self.power_menu && shell::power_menu_contains(h, self.pointer.x, self.pointer.y));
        let under = if over_chrome { None } else { surface_at(self, self.pointer) };
        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: self.pointer,
                serial,
                time: self.start.elapsed().as_millis() as u32,
            },
        );
        pointer.frame(self);
    }

    fn activate(&mut self, entry: Entry) {
        match entry {
            Entry::Start => {
                self.page = Page::Start;
                self.power_menu = false;
                self.close_wifi();
            }
            Entry::Voice => self.toggle_voice(),
            Entry::Network => {
                self.power_menu = false;
                if self.wifi.open {
                    self.close_wifi();
                } else {
                    self.wifi.open = true;
                    self.wifi.note = "Scanning".into();
                    self.start_wifi(WifiJobKind::Refresh);
                }
            }
            Entry::Power => {
                self.close_wifi();
                self.power_menu = !self.power_menu;
            }
            Entry::Browser => {
                self.power_menu = false;
                self.close_wifi();
                self.show_browser();
            }
            Entry::Page(index) => {
                self.power_menu = false;
                self.close_wifi();
                self.show_page(index);
            }
        }
        self.mark_dirty();
    }

    fn strip_hit(&self) -> Option<Entry> {
        shell::hit_strip(
            self.strip_expanded,
            self.output_size.h,
            self.pointer.x,
            self.pointer.y,
            self.pages.len(),
            self.page_scroll,
        )
    }

    fn show_browser(&mut self) {
        self.page = Page::Browser;
        if self.space.elements().next().is_none() {
            self.launch_browser();
        }
        self.focus_browser();
    }

    fn show_page(&mut self, index: usize) {
        self.page = Page::Browser;
        let slots = shell::page_slots(self.output_size.h);
        self.page_scroll = shell::reveal_page(self.page_scroll, index, slots, self.pages.len());
        if let Some(page) = self.pages.get(index) {
            let context = page.context.clone();
            self.active_context = Some(context.clone());
            if let Some(tx) = &self.pages_cmd {
                let _ = tx.send(PageCommand::Activate(context));
            }
        }
        if self.space.elements().next().is_none() {
            self.launch_browser();
        }
        self.focus_browser();
    }

    fn focus_browser(&mut self) {
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(window) = self.space.elements().last().cloned() {
            if let Some(top) = window.toplevel() {
                self.seat.get_keyboard().unwrap().set_focus(self, Some(top.wl_surface().clone()), serial);
            }
        }
    }

    fn launch_browser(&mut self) -> bool {
        let profile = pages::profile_dir();
        if let Err(err) = pages::ensure_profile(&profile) {
            tracing::error!(?err, "firefox profile");
        }
        let mut command = Command::new("firefox");
        command.args(pages::launch_args(&profile, self.browser_launched));
        match command
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .stdin(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {
                let first = !self.browser_launched;
                self.browser_launched = true;
                self.browser_starting = Some(Instant::now());
                self.arm_startup_feedback();
                if first {
                    self.start_pages();
                }
                true
            }
            Err(err) => {
                tracing::error!(?err, "failed to start firefox");
                false
            }
        }
    }

    fn start_pages(&mut self) {
        let Some(tx) = self.pages_tx.clone() else {
            self.pages_note = "Page list is not ready".into();
            return;
        };
        self.pages_generation = self.pages_generation.wrapping_add(1);
        let generation = self.pages_generation;
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        self.pages_cmd = Some(cmd_tx);
        let report_tx = tx.clone();
        if std::thread::Builder::new()
            .name("pages".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pages::run(cmd_rx, tx, generation)));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(note)) => {
                        let _ = report_tx.send(pages::Report::Stopped { generation, note });
                    }
                    Err(payload) => {
                        let _ = report_tx.send(pages::Report::Stopped {
                            generation,
                            note: pages::panic_note(payload.as_ref()),
                        });
                    }
                }
            })
            .is_err()
        {
            self.pages_note = "Could not list pages".into();
        }
    }

    pub fn browser_closed(&mut self) {
        self.page = Page::Start;
        self.browser_launched = false;
        self.browser_starting = None;
        self.pages.clear();
        self.active_context = None;
        self.page_scroll = 0;
        self.pages_generation = self.pages_generation.wrapping_add(1);
        self.pages_cmd = None;
        self.pages_note.clear();
        self.mark_dirty();
    }

    pub fn apply_pages(&mut self, report: pages::Report) {
        match report {
            pages::Report::Snapshot { generation, pages, active } => {
                if generation != self.pages_generation {
                    return;
                }
                self.pages = pages;
                self.active_context = active;
                let slots = shell::page_slots(self.output_size.h);
                self.page_scroll = shell::clamp_page_scroll(self.page_scroll, self.pages.len(), slots);
                self.pages_note.clear();
                self.mark_dirty();
            }
            pages::Report::Stopped { generation, note } => {
                if generation != self.pages_generation {
                    return;
                }
                tracing::warn!(%note, "page list stopped");
                self.pages.clear();
                self.active_context = None;
                self.pages_cmd = None;
                if !note.is_empty() {
                    self.pages_note = note;
                }
                self.mark_dirty();
            }
        }
    }

    fn scroll_pages(&mut self, amount: f64) {
        let slots = shell::page_slots(self.output_size.h);
        if self.pages.len() <= slots as usize {
            return;
        }
        self.page_scroll_carry += amount;
        let steps = (self.page_scroll_carry / 24.0).trunc();
        if steps == 0.0 {
            return;
        }
        self.page_scroll_carry -= steps * 24.0;
        self.page_scroll = shell::clamp_page_scroll(self.page_scroll + steps as i32, self.pages.len(), slots);
        self.mark_dirty();
    }

    pub fn apply_voice(&mut self, report: Report) {
        match report {
            Report::Status {
                generation,
                connecting,
                live,
                note,
            } => {
                if generation != self.voice.generation {
                    return;
                }
                self.voice.connecting = connecting;
                self.voice.live = live;
                self.voice.note = note;
                self.mark_dirty();
            }
            Report::Tool {
                generation,
                name,
                arguments,
                reply,
            } => self.voice_tool(generation, name, arguments, reply),
        }
    }

    fn toggle_voice(&mut self) {
        if self.voice.live || self.voice.connecting {
            self.stop_voice();
            return;
        }
        self.voice_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.voice_stop = std::sync::Arc::clone(&stop);
        self.voice.generation = self.voice.generation.wrapping_add(1);
        self.voice.connecting = true;
        self.voice.live = false;
        self.voice.note = "Connecting".into();
        let generation = self.voice.generation;
        let Some(tx) = self.voice_tx.clone() else {
            self.voice.connecting = false;
            self.voice.note = "Voice is not ready".into();
            self.mark_dirty();
            return;
        };
        self.mark_dirty();
        let report_tx = tx.clone();
        if std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    voice::run(tx, stop, generation);
                }));
                if let Err(payload) = result {
                    let _ = report_tx.send(voice::Report::Status {
                        generation,
                        connecting: false,
                        live: false,
                        note: voice::panic_note(payload.as_ref()),
                    });
                }
            })
            .is_err()
        {
            self.voice.connecting = false;
            self.voice.note = "Could not start voice".into();
        }
    }

    fn stop_voice(&mut self) {
        self.voice_stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.voice.generation = self.voice.generation.wrapping_add(1);
        self.voice.connecting = false;
        self.voice.live = false;
        self.voice.note.clear();
        self.mark_dirty();
    }

    fn voice_tool(
        &mut self,
        generation: u64,
        name: String,
        arguments: String,
        reply: tokio::sync::oneshot::Sender<String>,
    ) {
        if generation != self.voice.generation {
            let _ = reply.send("session ended".into());
            return;
        }
        match name.as_str() {
            "switch_entry" => {
                let _ = reply.send(self.voice_switch(&arguments));
            }
            "open_page" => self.voice_open(&arguments, reply),
            "end_voice_session" | "wait_for_user" => {
                let _ = reply.send("{\"ok\":true}".into());
            }
            "run_command" => {
                self.voice.note = crate::tools::command_preview(&arguments);
                self.mark_dirty();
                std::thread::spawn(move || {
                    let _ = reply.send(crate::tools::run_command_tool(&arguments));
                });
            }
            "wifi_radio" | "wifi_connect" | "wifi_disconnect" => {
                let tx = self.wifi_tx.clone();
                std::thread::spawn(move || {
                    let (text, report) = crate::tools::wifi_tool(&name, &arguments);
                    if let (Some(tx), Some(report)) = (tx, report) {
                        let _ = tx.send(report);
                    }
                    let _ = reply.send(text);
                });
            }
            "bluetooth_setup" => {
                self.voice.note = "Bluetooth".into();
                self.mark_dirty();
                std::thread::spawn(move || {
                    let _ = reply.send(crate::tools::bluetooth_tool(&arguments));
                });
            }
            _ => {
                let _ = reply.send("unknown tool".into());
            }
        }
    }

    fn voice_switch(&mut self, arguments: &str) -> String {
        let entry = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|value| value.get("entry").and_then(|entry| entry.as_str()).map(str::to_string))
            .unwrap_or_default();
        match entry.as_str() {
            "start" => {
                self.page = Page::Start;
                self.power_menu = false;
                self.mark_dirty();
                "showing start".into()
            }
            "browser" => {
                self.power_menu = false;
                self.show_browser();
                self.mark_dirty();
                "showing browser".into()
            }
            _ => {
                if let Some(index) = pages::pick(&self.pages, &entry) {
                    self.power_menu = false;
                    let label = pages::label(&self.pages[index].title, &self.pages[index].url);
                    self.show_page(index);
                    self.mark_dirty();
                    format!("showing {label}")
                } else {
                    "no matching page".into()
                }
            }
        }
    }

    fn voice_open(&mut self, arguments: &str, reply: tokio::sync::oneshot::Sender<String>) {
        let raw = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|value| value.get("url").and_then(|url| url.as_str()).map(str::to_string))
            .unwrap_or_default();
        let url = match pages::normalize_url(&raw) {
            Ok(url) => url,
            Err(err) => {
                let _ = reply.send(err.into());
                return;
            }
        };
        if self.pages_cmd.is_none() {
            if !self.browser_launched {
                if !self.launch_browser() {
                    let _ = reply.send("could not start the browser".into());
                    return;
                }
            } else {
                self.start_pages();
            }
        }
        self.page = Page::Browser;
        self.power_menu = false;
        self.focus_browser();
        self.mark_dirty();
        let Some(tx) = &self.pages_cmd else {
            let _ = reply.send("page list is not ready".into());
            return;
        };
        if let Err(err) = tx.send(PageCommand::Open { url, reply }) {
            if let PageCommand::Open { reply, .. } = err.0 {
                let _ = reply.send("page list is not ready".into());
            }
        }
    }

    pub fn apply_wifi(&mut self, report: wifi::WifiReport) {
        if report.generation != 0 && report.generation != self.wifi.generation {
            return;
        }
        self.wifi.busy = false;
        self.wifi.radio_on = report.radio_on;
        self.wifi.device = report.device;
        self.wifi.networks = report.networks;
        self.wifi.scroll = shell::clamp_wifi_scroll(self.wifi.scroll, self.wifi.networks.len());
        self.wifi.note = report.note;
        self.mark_dirty();
    }

    fn close_wifi(&mut self) {
        self.wifi.open = false;
        self.wifi.password_for = None;
        self.wifi.password.clear();
        self.strip_expanded = self.power_menu || self.pointer.x < f64::from(shell::STRIP_COLLAPSED);
        self.mark_dirty();
    }

    fn start_wifi(&mut self, kind: WifiJobKind) {
        let Some(tx) = self.wifi_tx.clone() else {
            self.wifi.note = "Wi-Fi is not ready".into();
            self.mark_dirty();
            return;
        };
        self.wifi.generation = self.wifi.generation.wrapping_add(1);
        self.wifi.busy = true;
        let generation = self.wifi.generation;
        let job = match kind {
            WifiJobKind::Refresh => WifiJob::Refresh { generation },
            WifiJobKind::Radio(on) => WifiJob::Radio { generation, on },
            WifiJobKind::Connect { ssid, password } => WifiJob::Connect { generation, ssid, password },
            WifiJobKind::Disconnect => WifiJob::Disconnect { generation },
        };
        self.mark_dirty();
        if std::thread::Builder::new()
            .name("wifi".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wifi::perform(job)));
                let report = match result {
                    Ok(report) => report,
                    Err(_) => wifi::WifiReport {
                        generation,
                        radio_on: None,
                        device: None,
                        networks: Vec::new(),
                        note: "Wi-Fi failed".into(),
                    },
                };
                let _ = tx.send(report);
            })
            .is_err()
        {
            self.wifi.busy = false;
            self.wifi.note = "Could not start the scan".into();
        }
    }

    fn wifi_hit(&mut self, hit: WifiHit) {
        match hit {
            WifiHit::Toggle => {
                let on = self.wifi.radio_on != Some(true);
                self.wifi.note = if on { "Turning on".into() } else { "Turning off".into() };
                self.start_wifi(WifiJobKind::Radio(on));
            }
            WifiHit::Scan => {
                self.wifi.note = "Scanning".into();
                self.start_wifi(WifiJobKind::Refresh);
            }
            WifiHit::Row(index) | WifiHit::Disconnect(index) => {
                let Some(net) = self.wifi.networks.get(index).cloned() else {
                    return;
                };
                if net.connected {
                    if matches!(hit, WifiHit::Disconnect(_)) {
                        self.wifi.note = "Disconnecting".into();
                        self.start_wifi(WifiJobKind::Disconnect);
                    }
                    return;
                }
                if net.secured {
                    self.wifi.password_for = Some(net.ssid);
                    self.wifi.password.clear();
                    self.mark_dirty();
                } else {
                    self.wifi.note = "Connecting".into();
                    self.start_wifi(WifiJobKind::Connect { ssid: net.ssid, password: None });
                }
            }
            WifiHit::Join => self.submit_wifi(),
        }
    }

    fn submit_wifi(&mut self) {
        let Some(ssid) = self.wifi.password_for.clone() else {
            return;
        };
        let password = std::mem::take(&mut self.wifi.password);
        self.wifi.password_for = None;
        let password = if password.is_empty() { None } else { Some(password) };
        self.wifi.note = "Connecting".into();
        self.start_wifi(WifiJobKind::Connect { ssid, password });
    }

    fn edit_password(&mut self, sym: u32, text: &str) {
        match wifi::password_edit(sym, text) {
            PasswordEdit::Cancel => {
                self.wifi.password_for = None;
                self.wifi.password.clear();
            }
            PasswordEdit::Submit => {
                self.submit_wifi();
                return;
            }
            PasswordEdit::Backspace => {
                wifi::pop_char(&mut self.wifi.password);
            }
            PasswordEdit::Insert(extra) => {
                if self.wifi.password.chars().count() < 128 {
                    self.wifi.password.push_str(&extra);
                }
            }
            PasswordEdit::Ignore => {}
        }
        self.mark_dirty();
    }

    fn scroll_wifi(&mut self, amount: f64) {
        self.wifi.scroll_carry += amount;
        let steps = (self.wifi.scroll_carry / 24.0).trunc();
        if steps == 0.0 {
            return;
        }
        self.wifi.scroll_carry -= steps * 24.0;
        self.wifi.scroll = shell::clamp_wifi_scroll(self.wifi.scroll + steps as i32, self.wifi.networks.len());
        self.mark_dirty();
    }

    fn run_power(&mut self, action: PowerAction) {
        self.power_menu = false;
        self.power_note.clear();
        match action {
            PowerAction::Logout => {
                tracing::info!("log out");
                std::process::exit(0);
            }
            PowerAction::Shutdown => self.finish_power("poweroff"),
            PowerAction::Reboot => self.finish_power("reboot"),
        }
        self.mark_dirty();
    }

    fn finish_power(&mut self, verb: &str) {
        // No polkit agent is running, so an interactive prompt cannot be answered.
        // --no-ask-password fails immediately instead of waiting on that prompt.
        match Command::new("loginctl").args(power_args(verb)).output() {
            Ok(output) if output.status.success() => tracing::info!(verb, "asked logind"),
            Ok(output) => {
                let note = power_failure(verb, &String::from_utf8_lossy(&output.stderr));
                tracing::error!(verb, note, "logind refused");
                self.power_note = note;
            }
            Err(err) => {
                let note = power_failure(verb, &err.to_string());
                tracing::error!(verb, note, "loginctl failed");
                self.power_note = note;
            }
        }
    }

    fn arm_startup_feedback(&mut self) {
        if self.startup_timer {
            return;
        }
        let Some(handle) = self.loop_handle.clone() else {
            return;
        };
        self.startup_timer = true;
        let timer = Timer::from_duration(Duration::from_millis(90));
        if handle
            .insert_source(timer, |_, _, state| {
                let waiting = state.browser_starting.is_some() && state.space.elements().next().is_none();
                if waiting {
                    state.mark_dirty();
                    TimeoutAction::ToDuration(Duration::from_millis(90))
                } else {
                    state.browser_starting = None;
                    state.startup_timer = false;
                    state.mark_dirty();
                    TimeoutAction::Drop
                }
            })
            .is_err()
        {
            self.startup_timer = false;
        }
    }
}

fn power_args(verb: &str) -> [&str; 2] {
    ["--no-ask-password", verb]
}

fn power_failure(verb: &str, detail: &str) -> String {
    let detail: String = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let detail: String = detail.chars().take(72).collect();
    if detail.is_empty() {
        format!("{verb} was refused")
    } else {
        detail
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollStep {
    Move(f64),
    Stop,
    Skip,
}

/// A finger or continuous scroll ends with a zero value. That zero is the stop,
/// not an empty frame. A wheel has no such terminator.
pub(crate) fn scroll_step(contact: bool, amount: Option<f64>) -> ScrollStep {
    match amount {
        Some(0.0) if contact => ScrollStep::Stop,
        Some(0.0) => ScrollStep::Skip,
        Some(amount) => ScrollStep::Move(amount),
        None => ScrollStep::Skip,
    }
}

/// libinput allows one scroll emulation at a time. Two fingers when the pad
/// reports them, otherwise a drag along the right or bottom edge.
pub(crate) fn preferred_scroll(methods: &[ScrollMethod]) -> Option<ScrollMethod> {
    if methods.contains(&ScrollMethod::TwoFinger) {
        Some(ScrollMethod::TwoFinger)
    } else if methods.contains(&ScrollMethod::Edge) {
        Some(ScrollMethod::Edge)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finger_scroll_ends_with_a_stop() {
        assert_eq!(scroll_step(true, Some(12.0)), ScrollStep::Move(12.0));
        assert_eq!(scroll_step(true, Some(0.0)), ScrollStep::Stop);
        assert_eq!(scroll_step(false, Some(0.0)), ScrollStep::Skip);
        assert_eq!(scroll_step(true, None), ScrollStep::Skip);
    }

    #[test]
    fn power_request_does_not_wait_for_a_password() {
        assert_eq!(power_args("poweroff"), ["--no-ask-password", "poweroff"]);
        assert_eq!(power_args("reboot"), ["--no-ask-password", "reboot"]);
        assert_eq!(
            power_failure("poweroff", "Interactive authentication required.\n"),
            "Interactive authentication required."
        );
        assert_eq!(power_failure("reboot", "   "), "reboot was refused");
    }

    #[test]
    fn edge_scroll_is_the_fallback() {
        assert_eq!(
            preferred_scroll(&[ScrollMethod::TwoFinger, ScrollMethod::Edge]),
            Some(ScrollMethod::TwoFinger)
        );
        assert_eq!(preferred_scroll(&[ScrollMethod::Edge]), Some(ScrollMethod::Edge));
        assert_eq!(preferred_scroll(&[ScrollMethod::NoScroll]), None);
    }
}
