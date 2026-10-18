use std::{ffi::OsString, time::Instant};

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{Seat, SeatState, keyboard::XkbConfig},
    output::Output,
    reexports::{
        calloop::LoopHandle,
        wayland_server::{
            DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
        },
    },
    utils::{Logical, Point, Size},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
    },
};

use crate::{keyboard, shell::Page};

pub struct State {
    pub display_handle: DisplayHandle,
    pub start: Instant,
    pub socket_name: OsString,

    pub space: Space<Window>,
    pub popups: PopupManager,
    pub output: Option<Output>,
    pub output_size: Size<i32, Logical>,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    // Kept so the wl_output and xdg-output globals stay advertised.
    #[allow(dead_code)]
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,

    pub pointer: Point<f64, Logical>,
    pub strip_expanded: bool,
    pub page: Page,
    pub browser_launched: bool,
    pub browser_starting: Option<Instant>,
    pub startup_timer: bool,
    pub loop_handle: Option<LoopHandle<'static, Self>>,
    pub strip_grab: bool,
    pub dirty: bool,
    pub frame_pending: bool,

    pub gpu: Option<crate::backend::Gpu>,
}

impl State {
    pub fn new(display: &mut smithay::reexports::wayland_server::Display<Self>, socket_name: OsString) -> Self {
        let display_handle = display.handle();
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "seat0");
        let layout = keyboard::load();
        tracing::info!(
            layout = %layout.layout,
            variant = %layout.variant,
            source = layout.source,
            "keyboard layout"
        );
        // Clients such as Firefox read the keymap from the seat. Setting the variables keeps
        // later processes on the same layout. This runs before any other thread exists.
        unsafe {
            std::env::set_var("XKB_DEFAULT_LAYOUT", &layout.layout);
            if !layout.variant.is_empty() {
                std::env::set_var("XKB_DEFAULT_VARIANT", &layout.variant);
            }
            if !layout.model.is_empty() {
                std::env::set_var("XKB_DEFAULT_MODEL", &layout.model);
            }
            if let Some(options) = &layout.options {
                std::env::set_var("XKB_DEFAULT_OPTIONS", options);
            }
        }
        let config = XkbConfig {
            rules: "",
            model: &layout.model,
            layout: &layout.layout,
            variant: &layout.variant,
            options: layout.options.clone(),
        };
        seat.add_keyboard(config, 200, 25).expect("keyboard");
        seat.add_pointer();

        Self {
            display_handle,
            start: Instant::now(),
            socket_name,
            space: Space::default(),
            popups: PopupManager::default(),
            output: None,
            output_size: (0, 0).into(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            seat,
            pointer: (0.0, 0.0).into(),
            strip_expanded: false,
            page: Page::Start,
            browser_launched: false,
            browser_starting: None,
            startup_timer: false,
            loop_handle: None,
            strip_grab: false,
            dirty: true,
            frame_pending: false,
            gpu: None,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
