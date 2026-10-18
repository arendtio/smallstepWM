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
    shell::{self, Entry, Page, PowerAction},
    state::State,
};

impl State {
    pub fn pointer_moved(&mut self, pos: Point<f64, Logical>) {
        let size = self.output_size;
        let pos = Point::from((
            pos.x.clamp(0.0, (size.w.saturating_sub(1).max(0)) as f64),
            pos.y.clamp(0.0, (size.h.saturating_sub(1).max(0)) as f64),
        ));
        let over_strip = pos.x < f64::from(shell::strip_width(self.strip_expanded));
        let expanded = self.power_menu || over_strip;
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
                self.seat.get_keyboard().unwrap().input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |_, _, _| {
                        let _ = KeyState::Released;
                        FilterResult::Forward
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
                if self.strip_grab || shell::hit_strip(self.strip_expanded, self.output_size.h, self.pointer.x, self.pointer.y).is_some()
                {
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
        let on_strip = shell::hit_strip(self.strip_expanded, self.output_size.h, self.pointer.x, self.pointer.y);
        if state == ButtonState::Pressed {
            if let Some(entry) = on_strip {
                self.strip_grab = true;
                self.activate(entry);
                return;
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
        let under = if shell::hit_strip(self.strip_expanded, self.output_size.h, self.pointer.x, self.pointer.y).is_some() {
            None
        } else {
            surface_at(self, self.pointer)
        };
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
            }
            Entry::Power => self.power_menu = !self.power_menu,
            Entry::Browser => {
                self.power_menu = false;
                self.page = Page::Browser;
                if self.space.elements().next().is_none() {
                    self.launch_browser();
                }
                let serial = SERIAL_COUNTER.next_serial();
                if let Some(window) = self.space.elements().last().cloned() {
                    if let Some(top) = window.toplevel() {
                        self.seat.get_keyboard().unwrap().set_focus(
                            self,
                            Some(top.wl_surface().clone()),
                            serial,
                        );
                    }
                }
            }
        }
        self.mark_dirty();
    }

    fn launch_browser(&mut self) {
        let mut command = Command::new("firefox");
        if self.browser_launched {
            command.arg("--new-window");
        }
        match command
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .stdin(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {
                self.browser_launched = true;
                self.browser_starting = Some(Instant::now());
                self.arm_startup_feedback();
            }
            Err(err) => tracing::error!(?err, "failed to start firefox"),
        }
    }

    fn run_power(&mut self, action: PowerAction) {
        self.power_menu = false;
        self.mark_dirty();
        match action {
            PowerAction::Logout => {
                tracing::info!("log out");
                std::process::exit(0);
            }
            PowerAction::Shutdown => request_session("poweroff"),
            PowerAction::Reboot => request_session("reboot"),
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

fn request_session(verb: &str) {
    match Command::new("loginctl").arg(verb).status() {
        Ok(status) if status.success() => tracing::info!(verb, "asked logind"),
        Ok(status) => tracing::error!(verb, ?status, "logind refused"),
        Err(err) => tracing::error!(verb, ?err, "loginctl failed"),
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
    fn edge_scroll_is_the_fallback() {
        assert_eq!(
            preferred_scroll(&[ScrollMethod::TwoFinger, ScrollMethod::Edge]),
            Some(ScrollMethod::TwoFinger)
        );
        assert_eq!(preferred_scroll(&[ScrollMethod::Edge]), Some(ScrollMethod::Edge));
        assert_eq!(preferred_scroll(&[ScrollMethod::NoScroll]), None);
    }
}
