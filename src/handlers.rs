use smithay::{
    desktop::{PopupKind, WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords},
    input::{SeatHandler, SeatState},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client,
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
            with_states,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler,
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::{
    shell::Page,
    state::{ClientState, State},
};

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        smithay::backend::renderer::utils::on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|window| window.toplevel().is_some_and(|top| top.wl_surface() == &root))
                .cloned()
            {
                window.on_commit();
            }
        }
        self.popups.commit(surface);
        if let Some(window) = self
            .space
            .elements()
            .find(|window| window.toplevel().is_some_and(|top| top.wl_surface() == surface))
            .cloned()
        {
            let initial = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .initial_configure_sent
            });
            if !initial {
                window.toplevel().unwrap().send_configure();
            }
        }
        self.mark_dirty();
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let size = self.output_size;
        surface.with_pending_state(|state| {
            state.size = Some(size);
            state.states.set(xdg_toplevel::State::Maximized);
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_pending_configure();
        let window = smithay::desktop::Window::new_wayland_window(surface);
        self.space.map_element(window.clone(), (0, 0), true);
        self.page = Page::Browser;
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        if let Some(top) = window.toplevel() {
            self.seat.get_keyboard().unwrap().set_focus(
                self,
                Some(top.wl_surface().clone()),
                serial,
            );
        }
        self.mark_dirty();
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        if let Some(root) = find_popup_root_surface(&PopupKind::Xdg(surface.clone())).ok() {
            if let Some(window) = self
                .space
                .elements()
                .find(|window| window.toplevel().is_some_and(|top| top.wl_surface() == &root))
            {
                let output_geo = self
                    .output
                    .as_ref()
                    .and_then(|output| self.space.output_geometry(output))
                    .unwrap_or_else(|| smithay::utils::Rectangle::from_size(self.output_size));
                let window_geo = self.space.element_geometry(window).unwrap_or_default();
                let mut target = output_geo;
                target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(surface.clone()));
                target.loc -= window_geo.loc;
                surface.with_pending_state(|state| {
                    state.geometry = state.positioner.get_unconstrained_geometry(target);
                });
            }
        }
        let _ = surface.send_configure();
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
        self.mark_dirty();
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        let _ = surface.send_configure();
        self.mark_dirty();
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl = surface.wl_surface().clone();
        let window = self
            .space
            .elements()
            .find(|window| window.toplevel().is_some_and(|top| top.wl_surface() == &wl))
            .cloned();
        if let Some(window) = window {
            self.space.unmap_elem(&window);
        }
        if self.space.elements().next().is_none() {
            self.page = Page::Start;
        }
        self.mark_dirty();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}
impl OutputHandler for State {}

pub fn surface_at(state: &State, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
    if state.page != Page::Browser {
        return None;
    }
    let (window, location) = state.space.element_under(pos)?;
    window
        .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
        .map(|(surface, offset)| (surface, (offset + location).to_f64()))
}

smithay::delegate_compositor!(State);
smithay::delegate_shm!(State);
smithay::delegate_xdg_shell!(State);
smithay::delegate_seat!(State);
smithay::delegate_data_device!(State);
smithay::delegate_output!(State);
