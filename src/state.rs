use std::{
    ffi::OsString,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{Seat, SeatState, keyboard::XkbConfig},
    output::Output,
    reexports::{
        calloop::{channel::Sender, LoopHandle},
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

use crate::{keyboard, pages, shell::Page, voice, wifi};

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
    pub power_menu: bool,
    pub wifi: wifi::Panel,
    pub wifi_tx: Option<Sender<wifi::WifiReport>>,
    pub voice: voice::Session,
    pub voice_tx: Option<Sender<voice::Report>>,
    pub voice_stop: Arc<AtomicBool>,
    pub voice_f1_down: bool,
    pub browser_launched: bool,
    pub browser_starting: Option<Instant>,
    pub pages: Vec<pages::Page>,
    pub active_context: Option<String>,
    pub page_scroll: i32,
    pub page_scroll_carry: f64,
    pub pages_cmd: Option<tokio::sync::mpsc::UnboundedSender<pages::Command>>,
    pub pages_tx: Option<Sender<pages::Report>>,
    pub pages_generation: u64,
    pub pages_note: String,
    pub power_note: String,
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
            power_menu: false,
            wifi: wifi::Panel::default(),
            wifi_tx: None,
            voice: voice::Session::default(),
            voice_tx: None,
            voice_stop: Arc::new(AtomicBool::new(false)),
            voice_f1_down: false,
            browser_launched: false,
            browser_starting: None,
            pages: Vec::new(),
            active_context: None,
            page_scroll: 0,
            page_scroll_carry: 0.0,
            pages_cmd: None,
            pages_tx: None,
            pages_generation: 0,
            pages_note: String::new(),
            power_note: String::new(),
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
