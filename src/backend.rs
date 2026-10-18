use std::{io, time::Duration};

use smithay::{
    backend::{
        allocator::{
            Allocator, Fourcc, Modifier,
            dumb::{DumbAllocator, DumbBuffer},
            gbm::GbmDevice,
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent,
            compositor::FrameFlags,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
            renderer::{
            Color32F, ImportAll, ImportDma, ImportMem,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            pixman::PixmanRenderer,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev,
    },
    desktop::PopupManager,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode as CalloopMode, PostAction, generic::Generic},
        drm::control::{self, Device as ControlDevice, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Point, Transform},
    wayland::socket::ListeningSocketSource,
};

#[derive(Debug, Clone)]
pub(crate) struct SharedDumb(DrmDeviceFd);

impl Allocator for SharedDumb {
    type Buffer = DumbBuffer;
    type Error = io::Error;

    fn create_buffer(
        &mut self,
        width: u32,
        height: u32,
        fourcc: Fourcc,
        modifiers: &[Modifier],
    ) -> Result<DumbBuffer, io::Error> {
        DumbAllocator::new(self.0.clone()).create_buffer(width, height, fourcc, modifiers)
    }
}

use crate::{
    shell::{self, Page},
    state::{ClientState, State},
};

pub struct Gpu {
    pub renderer: PixmanRenderer,
    pub drm: DrmOutputManager<SharedDumb, DrmDeviceFd, (), DrmDeviceFd>,
    pub surface: DrmOutput<SharedDumb, DrmDeviceFd, (), DrmDeviceFd>,
    pub background: MemoryRenderBuffer,
    pub strip: MemoryRenderBuffer,
    pub strip_key: (bool, Page, i32, i32),
    pub cursor: SolidColorBuffer,
    pub crtc: crtc::Handle,
}

smithay::backend::renderer::element::render_elements! {
    pub ShellElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
    Solid = SolidColorRenderElement,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let mut display: Display<State> = Display::new()?;
    let socket_name = listen(&mut event_loop, &mut display)?;
    let mut state = State::new(&mut display, socket_name);
    // SAFETY: the display source lives as long as the loop, and dispatch does not drop it.
    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                unsafe {
                    display.get_mut().dispatch_clients(state)?;
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| err.to_string())?;

    let handle = event_loop.handle();
    state.gpu = Some(open_gpu(&mut state, &handle)?);
    // The process is still single-threaded here. Children such as Firefox inherit this.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
        std::env::set_var("MOZ_ENABLE_WAYLAND", "1");
    }
    tracing::info!(socket = ?state.socket_name, "session ready");

    pump(&mut state);
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
        pump(state);
    })?;
    Ok(())
}

fn listen(
    event_loop: &mut EventLoop<State>,
    _display: &mut Display<State>,
) -> Result<std::ffi::OsString, Box<dyn std::error::Error>> {
    let listening = ListeningSocketSource::new_auto()?;
    let name = listening.socket_name().to_os_string();
    event_loop
        .handle()
        .insert_source(listening, |client, _, state| {
            if let Err(err) = state
                .display_handle
                .insert_client(client, std::sync::Arc::new(ClientState::default()))
            {
                tracing::error!(?err, "client rejected");
            }
        })
        .map_err(|err| err.to_string())?;
    Ok(name)
}

fn open_gpu(
    state: &mut State,
    handle: &LoopHandle<'static, State>,
) -> Result<Gpu, Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new().map_err(|err| err.to_string())?;
    let seat_name = session.seat();
    let path = udev::primary_gpu(&seat_name)?.ok_or("no DRM device on this seat")?;

    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| "failed to assign the libinput seat")?;
    let libinput_saved = libinput.clone();
    handle
        .insert_source(LibinputInputBackend::new(libinput), |event, _, state| {
            state.on_input(event);
        })
        .map_err(|err| err.to_string())?;

    let mut libinput_for_session = libinput_saved.clone();
    handle
        .insert_source(notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                libinput_for_session.suspend();
                if let Some(gpu) = state.gpu.as_mut() {
                    gpu.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                if let Err(err) = libinput_for_session.resume() {
                    tracing::error!(?err, "libinput resume");
                }
                if let Some(gpu) = state.gpu.as_mut() {
                    if let Err(err) = gpu.drm.activate(false) {
                        tracing::error!(?err, "drm resume");
                    }
                }
                state.frame_pending = false;
                state.mark_dirty();
            }
        })
        .map_err(|err| err.to_string())?;

    let fd = session
        .open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)
        .map_err(|err| err.to_string())?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    handle
        .insert_source(drm_notifier, |event, _, state| {
            if let DrmEvent::VBlank(crtc) = event {
                on_vblank(state, crtc);
            }
        })
        .map_err(|err| err.to_string())?;

    let picked = pick_output(&drm).ok_or("no connected display")?;
    let mode = wl_mode(&picked.mode);
    let output = Output::new(
        picked.name.clone(),
        PhysicalProperties {
            size: (picked.mm.0 as i32, picked.mm.1 as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Smallstep".into(),
            model: picked.name,
        },
    );
    let _global = output.create_global::<State>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output_size = mode.size.to_logical(1);
    state.output = Some(output.clone());

    let mut renderer = PixmanRenderer::new().map_err(|err| err.to_string())?;
    let renderer_formats = renderer.dmabuf_formats();
    let gbm: Option<GbmDevice<DrmDeviceFd>> = None;
    let mut drm_manager = DrmOutputManager::new(
        drm,
        SharedDumb(fd.clone()),
        fd,
        gbm,
        [Fourcc::Xrgb8888],
        renderer_formats,
    );
    let elements = DrmOutputRenderElements::<PixmanRenderer, ShellElement<PixmanRenderer>>::default();
    let surface = drm_manager.initialize_output(
        picked.crtc,
        picked.mode,
        &[picked.connector],
        &output,
        None,
        &mut renderer,
        &elements,
    )?;

    let background = background_buffer(state.output_size.w, state.output_size.h);
    let strip = strip_buffer(state.output_size.h, false, Page::Start);
    Ok(Gpu {
        renderer,
        drm: drm_manager,
        surface,
        background,
        strip,
        strip_key: (false, Page::Start, state.output_size.w, state.output_size.h),
        cursor: SolidColorBuffer::new((8, 8), Color32F::new(0.93, 0.93, 0.9, 1.0)),
        crtc: picked.crtc,
    })
}

fn on_vblank(state: &mut State, crtc: crtc::Handle) {
    let Some(gpu) = state.gpu.as_mut() else {
        return;
    };
    if gpu.crtc != crtc {
        return;
    }
    if let Err(err) = gpu.surface.frame_submitted() {
        tracing::warn!(?err, "frame submitted");
    }
    state.frame_pending = false;
    if let Some(output) = state.output.clone() {
        let time = state.start.elapsed();
        for window in state.space.elements() {
            window.send_frame(&output, time, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
    }
    state.space.refresh();
    state.popups.cleanup();
}

pub fn pump(state: &mut State) {
    if !state.dirty || state.frame_pending || state.gpu.is_none() {
        return;
    }
    match draw(state) {
        Ok(true) => {
            state.dirty = false;
            state.frame_pending = true;
        }
        Ok(false) => state.dirty = false,
        Err(err) => {
            tracing::error!(?err, "draw");
            state.dirty = false;
        }
    }
}

fn draw(state: &mut State) -> Result<bool, Box<dyn std::error::Error>> {
    let page = state.page;
    let expanded = state.strip_expanded;
    let size = state.output_size;
    let pointer = state.pointer;
    let windows: Vec<_> = state.space.elements().cloned().collect();

    let gpu = state.gpu.as_mut().ok_or("no gpu")?;
    if gpu.strip_key != (expanded, page, size.w, size.h) {
        gpu.strip = strip_buffer(size.h, expanded, page);
        gpu.strip_key = (expanded, page, size.w, size.h);
    }

    let mut elements: Vec<ShellElement<PixmanRenderer>> = Vec::new();
    let cursor_at = Point::<i32, smithay::utils::Physical>::from((
        (pointer.x as i32 - 4).max(0),
        (pointer.y as i32 - 4).max(0),
    ));
    elements.push(ShellElement::Solid(SolidColorRenderElement::from_buffer(
        &gpu.cursor,
        cursor_at,
        1.0,
        1.0,
        Kind::Cursor,
    )));
    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            &mut gpu.renderer,
            (0.0, 0.0),
            &gpu.strip,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map_err(|err| err.to_string())?,
    ));

    if page == Page::Browser {
        if let Some(window) = windows.last() {
            if let Some(top) = window.toplevel() {
                let surfaces = render_elements_from_surface_tree::<_, ShellElement<_>>(
                    &mut gpu.renderer,
                    top.wl_surface(),
                    (0, 0),
                    1.0,
                    1.0,
                    Kind::Unspecified,
                );
                elements.extend(surfaces);
                for (popup, loc) in PopupManager::popups_for_surface(top.wl_surface()) {
                    let at = Point::<i32, smithay::utils::Physical>::from((loc.x, loc.y));
                    elements.extend(render_elements_from_surface_tree::<_, ShellElement<_>>(
                        &mut gpu.renderer,
                        popup.wl_surface(),
                        at,
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    ));
                }
            }
        }
    }

    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            &mut gpu.renderer,
            (0.0, 0.0),
            &gpu.background,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map_err(|err| err.to_string())?,
    ));

    // render_frame walks front to back: cursor, strip, window, then the gradient.
    let result = gpu.surface.render_frame(
        &mut gpu.renderer,
        &elements,
        Color32F::new(0.08, 0.09, 0.11, 1.0),
        FrameFlags::empty(),
    );
    match result {
        Ok(_) => match gpu.surface.queue_frame(()) {
            Ok(()) => Ok(true),
            Err(err) => {
                let text = err.to_string();
                if text.contains("Empty") {
                    Ok(false)
                } else {
                    Err(text.into())
                }
            }
        },
        Err(err) => Err(err.to_string().into()),
    }
}

fn background_buffer(width: i32, height: i32) -> MemoryRenderBuffer {
    let pixels = shell::fill_gradient(width, height);
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Xrgb8888,
        (width, height),
        1,
        Transform::Normal,
        None,
    )
}

fn strip_buffer(height: i32, expanded: bool, page: Page) -> MemoryRenderBuffer {
    let width = shell::strip_width(expanded);
    let pixels = shell::paint_strip(width, height, expanded, page);
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Xrgb8888,
        (width, height),
        1,
        Transform::Normal,
        None,
    )
}

struct Picked {
    name: String,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: control::Mode,
    mm: (u32, u32),
}

fn pick_output(device: &DrmDevice) -> Option<Picked> {
    let resources = device.resource_handles().ok()?;
    let mut best: Option<(u8, Picked)> = None;
    for handle in resources.connectors() {
        let Ok(info) = device.get_connector(*handle, true) else {
            continue;
        };
        if info.state() != connector::State::Connected {
            continue;
        }
        let Some(mode) = info
            .modes()
            .iter()
            .copied()
            .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| info.modes().first().copied())
        else {
            continue;
        };
        let Some(crtc) = find_crtc(device, &resources, &info) else {
            continue;
        };
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let rank = match info.interface().as_str() {
            "eDP" | "LVDS" | "DSI" => 0,
            "DP" => 1,
            _ => 2,
        };
        let picked = Picked {
            name,
            connector: *handle,
            crtc,
            mode,
            mm: info.size().unwrap_or((0, 0)),
        };
        if best.as_ref().is_none_or(|(score, _)| rank < *score) {
            best = Some((rank, picked));
        }
    }
    best.map(|(_, picked)| picked)
}

fn find_crtc(
    device: &DrmDevice,
    resources: &control::ResourceHandles,
    info: &connector::Info,
) -> Option<crtc::Handle> {
    for enc_handle in info.encoders() {
        let Ok(encoder) = device.get_encoder(*enc_handle) else {
            continue;
        };
        if let Some(crtc) = resources.filter_crtcs(encoder.possible_crtcs()).into_iter().next() {
            return Some(crtc);
        }
    }
    None
}

fn wl_mode(mode: &control::Mode) -> Mode {
    let (w, h) = mode.size();
    let refresh = mode.vrefresh();
    Mode {
        size: (i32::from(w), i32::from(h)).into(),
        refresh: if refresh == 0 { 60_000 } else { (refresh as i32) * 1000 },
    }
}
