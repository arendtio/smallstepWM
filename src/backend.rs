use std::time::Duration;

use smithay::{
    reexports::input::Device as InputDevice,
    backend::{
        allocator::Fourcc,
        drm::{DrmDevice, DrmDeviceFd, DrmEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Bind, Color32F, ImportAll, ImportMem,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::WaylandSurfaceRenderElement,
            },
            pixman::PixmanRenderer,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode as CalloopMode, PostAction, generic::Generic},
        drm::{
            buffer::Buffer as DrmBuffer,
            control::{
                self, Device as ControlDevice, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer,
                framebuffer,
            },
        },
        input::Libinput,
        pixman,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Point, Scale, Transform},
    wayland::socket::ListeningSocketSource,
};

pub(crate) struct ScanBuffer {
    dumb: dumbbuffer::DumbBuffer,
    fb: framebuffer::Handle,
}

use crate::{
    shell::{self, Page},
    state::{ClientState, State},
};

pub struct Gpu {
    pub renderer: PixmanRenderer,
    pub drm: DrmDevice,
    pub fd: DrmDeviceFd,
    pub buffers: Vec<ScanBuffer>,
    pub damage: OutputDamageTracker,
    pub background: MemoryRenderBuffer,
    pub strip: MemoryRenderBuffer,
    pub strip_key: (bool, Page, bool, bool, bool, i32, i32),
    pub power_menu: MemoryRenderBuffer,
    pub cursor: MemoryRenderBuffer,
    pub cursor_hot: (i32, i32),
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub mode: control::Mode,
    pub displayed: Option<usize>,
    pub pending: Option<usize>,
}

smithay::backend::renderer::element::render_elements! {
    pub ShellElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let mut display: Display<State> = Display::new()?;
    let socket_name = listen(&mut event_loop, &mut display)?;
    let mut state = State::new(&mut display, socket_name);
    state.loop_handle = Some(event_loop.handle());
    let (wifi_tx, wifi_channel) = smithay::reexports::calloop::channel::channel();
    state.wifi_tx = Some(wifi_tx);
    event_loop
        .handle()
        .insert_source(wifi_channel, |event, _, state| {
            if let smithay::reexports::calloop::channel::Event::Msg(report) = event {
                state.apply_wifi(report);
            }
        })
        .map_err(|err| err.to_string())?;
    let (voice_tx, voice_channel) = smithay::reexports::calloop::channel::channel();
    state.voice_tx = Some(voice_tx);
    event_loop
        .handle()
        .insert_source(voice_channel, |event, _, state| {
            if let smithay::reexports::calloop::channel::Event::Msg(report) = event {
                state.apply_voice(report);
            }
        })
        .map_err(|err| err.to_string())?;
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

    pump(&mut state)?;
    if state.gpu.as_ref().is_some_and(|gpu| gpu.displayed.is_none()) {
        return Err("the first frame was not shown".into());
    }
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
        if let Err(err) = pump(state) {
            tracing::error!(%err, "draw");
        }
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
            match event {
                smithay::backend::input::InputEvent::DeviceAdded { mut device } => {
                    configure_pointer_scroll(&mut device);
                }
                other => state.on_input(other),
            }
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
                    gpu.pending = None;
                    if let Err(err) = restore_scanout(gpu) {
                        tracing::error!(?err, "restore scanout");
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

    let renderer = PixmanRenderer::new().map_err(|err| err.to_string())?;
    let (width, height) = picked.mode.size();
    let buffers = alloc_scan_buffers(&fd, (u32::from(width), u32::from(height)))?;
    let damage = OutputDamageTracker::from_output(&output);
    let background = background_buffer(state.output_size.w, state.output_size.h);
    let strip = strip_buffer(state.output_size.h, false, Page::Start, false, false, false);
    let (cursor, cursor_hot) = cursor_buffer();
    Ok(Gpu {
        renderer,
        drm,
        fd,
        buffers,
        damage,
        background,
        strip,
        strip_key: (false, Page::Start, false, false, false, state.output_size.w, state.output_size.h),
        power_menu: power_menu_buffer(),
        cursor,
        cursor_hot,
        crtc: picked.crtc,
        connector: picked.connector,
        mode: picked.mode,
        displayed: None,
        pending: None,
    })
}

fn alloc_scan_buffers(fd: &DrmDeviceFd, size: (u32, u32)) -> Result<Vec<ScanBuffer>, Box<dyn std::error::Error>> {
    let mut buffers = Vec::new();
    for index in 0..2 {
        match alloc_scan_buffer(fd, size) {
            Ok(buffer) => buffers.push(buffer),
            Err(err) if index == 0 => {
                return Err(format!("creating the scanout buffer failed: {err}").into());
            }
            Err(err) => {
                tracing::warn!(?err, "second scanout buffer unavailable, using setcrtc");
                break;
            }
        }
    }
    Ok(buffers)
}

fn alloc_scan_buffer(fd: &DrmDeviceFd, size: (u32, u32)) -> std::io::Result<ScanBuffer> {
    let dumb = fd.create_dumb_buffer(size, Fourcc::Xrgb8888, 32)?;
    match fd.add_framebuffer(&dumb, 24, 32) {
        Ok(fb) => Ok(ScanBuffer { dumb, fb }),
        Err(err) => {
            let _ = fd.destroy_dumb_buffer(dumb);
            Err(err)
        }
    }
}

fn on_vblank(state: &mut State, crtc: crtc::Handle) {
    let Some(gpu) = state.gpu.as_mut() else {
        return;
    };
    if gpu.pending.is_none() && gpu.crtc != crtc {
        return;
    }
    if let Some(index) = gpu.pending.take() {
        gpu.displayed = Some(index);
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

pub fn pump(state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    if !state.dirty || state.frame_pending || state.gpu.is_none() {
        return Ok(());
    }
    match draw(state) {
        Ok(true) => {
            state.dirty = false;
            state.frame_pending = true;
            Ok(())
        }
        Ok(false) => {
            state.dirty = false;
            Ok(())
        }
        Err(err) => {
            state.dirty = false;
            Err(err)
        }
    }
}

fn draw(state: &mut State) -> Result<bool, Box<dyn std::error::Error>> {
    let page = state.page;
    let expanded = state.strip_expanded;
    let size = state.output_size;
    let pointer = state.pointer;
    let windows: Vec<_> = state
        .space
        .elements()
        .filter_map(|window| {
            let loc = state.space.element_location(window)?;
            Some((window.clone(), loc))
        })
        .collect();
    let startup = if windows.is_empty() {
        state
            .browser_starting
            .map(|started| shell::startup_phase(started.elapsed().as_millis()))
    } else {
        None
    };
    let power_menu = state.power_menu;
    let network_open = state.wifi.open;
    let voice_live = state.voice.live;
    let voice_note = if state.voice.note.is_empty() {
        None
    } else {
        Some(shell::paint_voice_note(&state.voice.note))
    };
    let wifi_pixels = if network_open {
        let rows: Vec<shell::WifiPaint<'_>> = state
            .wifi
            .networks
            .iter()
            .map(|net| shell::WifiPaint {
                name: &net.ssid,
                signal: net.signal,
                secured: net.secured,
                connected: net.connected,
            })
            .collect();
        let password = state.wifi.password_for.as_ref().map(|_| state.wifi.password.as_str());
        Some(shell::paint_wifi_panel(
            &rows,
            state.wifi.scroll,
            state.wifi.radio_on,
            &state.wifi.note,
            password,
        ))
    } else {
        None
    };

    let gpu = state.gpu.as_mut().ok_or("no gpu")?;
    if gpu.strip_key != (expanded, page, network_open, power_menu, voice_live, size.w, size.h) {
        gpu.strip = strip_buffer(size.h, expanded, page, network_open, power_menu, voice_live);
        gpu.strip_key = (expanded, page, network_open, power_menu, voice_live, size.w, size.h);
    }

    let mut elements: Vec<ShellElement<PixmanRenderer>> = Vec::new();
    let cursor_at = (
        pointer.x as i32 - gpu.cursor_hot.0,
        pointer.y as i32 - gpu.cursor_hot.1,
    );
    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            &mut gpu.renderer,
            (f64::from(cursor_at.0), f64::from(cursor_at.1)),
            &gpu.cursor,
            None,
            None,
            None,
            Kind::Cursor,
        )
        .map_err(|err| err.to_string())?,
    ));
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
    if let Some((pixels, width, height)) = &voice_note {
        let note = MemoryRenderBuffer::from_slice(
            pixels,
            Fourcc::Argb8888,
            (*width, *height),
            1,
            Transform::Normal,
            None,
        );
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                &mut gpu.renderer,
                (f64::from(shell::STRIP_COLLAPSED + 8), 8.0),
                &note,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .map_err(|err| err.to_string())?,
        ));
    }
    if let Some(pixels) = &wifi_pixels {
        let (x, y) = shell::wifi_origin(size.h);
        let panel = MemoryRenderBuffer::from_slice(
            pixels,
            Fourcc::Argb8888,
            (shell::WIFI_W, shell::WIFI_H),
            1,
            Transform::Normal,
            None,
        );
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                &mut gpu.renderer,
                (f64::from(x), f64::from(y)),
                &panel,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .map_err(|err| err.to_string())?,
        ));
    }
    if power_menu {
        let (x, y) = shell::power_menu_origin(size.h);
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                &mut gpu.renderer,
                (f64::from(x), f64::from(y)),
                &gpu.power_menu,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .map_err(|err| err.to_string())?,
        ));
    }
    if let Some(phase) = startup {
        let pixels = shell::paint_startup_bar(phase);
        let bar = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (shell::BAR_W, shell::BAR_H),
            1,
            Transform::Normal,
            None,
        );
        let content_w = (size.w - shell::STRIP_COLLAPSED).max(1);
        let x = shell::STRIP_COLLAPSED + (content_w - shell::BAR_W) / 2;
        let y = (size.h - shell::BAR_H) / 2;
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                &mut gpu.renderer,
                (f64::from(x), f64::from(y)),
                &bar,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .map_err(|err| err.to_string())?,
        ));
    }

    if page == Page::Browser {
        if let Some((window, loc)) = windows.last() {
            let at = Point::<i32, smithay::utils::Physical>::from((loc.x, loc.y));
            elements.extend(window.render_elements(
                &mut gpu.renderer,
                at,
                Scale::from(1.0),
                1.0,
            ));
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

    // Front to back: cursor, strip, Wi-Fi panel, power menu, startup bar, window, then the gradient.
    let index = gpu.displayed.map(|shown| (shown + 1) % gpu.buffers.len()).unwrap_or(0);
    paint_scanout(gpu, index, &elements)?;
    present(gpu, index)
}

fn paint_scanout(
    gpu: &mut Gpu,
    index: usize,
    elements: &[ShellElement<PixmanRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = DrmBuffer::size(&gpu.buffers[index].dumb);
    let pitch = DrmBuffer::pitch(&gpu.buffers[index].dumb) as usize;
    let mut mapping = gpu.fd.map_dumb_buffer(&mut gpu.buffers[index].dumb).map_err(|err| {
        format!(
            "mapping the dumb buffer failed: {err} (os error {:?})",
            err.raw_os_error()
        )
    })?;
    let format = pixman::FormatCode::try_from(Fourcc::Xrgb8888).map_err(|_| "unsupported scanout format")?;
    // The mapping stays alive until this function returns, after the image is dropped.
    let mut image = unsafe {
        pixman::Image::from_raw_mut(
            format,
            width as usize,
            height as usize,
            mapping.as_mut_ptr() as *mut u32,
            pitch,
            false,
        )
    }
    .map_err(|_| "creating the scanout image failed")?;
    let mut target = gpu.renderer.bind(&mut image).map_err(|err| err.to_string())?;
    gpu.damage
        .render_output(
            &mut gpu.renderer,
            &mut target,
            0,
            elements,
            Color32F::new(0.08, 0.09, 0.11, 1.0),
        )
        .map_err(|err| err.to_string())?;
    drop(target);
    drop(image);
    drop(mapping);
    Ok(())
}

fn present(gpu: &mut Gpu, index: usize) -> Result<bool, Box<dyn std::error::Error>> {
    let fb = gpu.buffers[index].fb;
    if gpu.displayed.is_none() || gpu.buffers.len() == 1 {
        gpu.fd
            .set_crtc(gpu.crtc, Some(fb), (0, 0), &[gpu.connector], Some(gpu.mode))
            .map_err(|err| format!("setting the display mode failed: {err} (os error {:?})", err.raw_os_error()))?;
        gpu.displayed = Some(index);
        return Ok(false);
    }
    match gpu.fd.page_flip(gpu.crtc, fb, PageFlipFlags::EVENT, None) {
        Ok(()) => {
            gpu.pending = Some(index);
            Ok(true)
        }
        Err(err) if err.raw_os_error() == Some(95) || err.raw_os_error() == Some(22) => {
            gpu.fd
                .set_crtc(gpu.crtc, Some(fb), (0, 0), &[gpu.connector], Some(gpu.mode))
                .map_err(|err| format!("setting the display mode failed: {err}"))?;
            gpu.displayed = Some(index);
            Ok(false)
        }
        Err(err) => Err(format!("page flip failed: {err} (os error {:?})", err.raw_os_error()).into()),
    }
}

fn restore_scanout(gpu: &mut Gpu) -> Result<(), String> {
    let Some(index) = gpu.displayed else {
        return Ok(());
    };
    let fb = gpu.buffers[index].fb;
    gpu.fd
        .set_crtc(gpu.crtc, Some(fb), (0, 0), &[gpu.connector], Some(gpu.mode))
        .map_err(|err| format!("setting the display mode failed: {err} (os error {:?})", err.raw_os_error()))
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

fn cursor_buffer() -> (MemoryRenderBuffer, (i32, i32)) {
    let cursor = shell::paint_cursor();
    let buffer = MemoryRenderBuffer::from_slice(
        &cursor.pixels,
        Fourcc::Argb8888,
        (cursor.width, cursor.height),
        1,
        Transform::Normal,
        None,
    );
    (buffer, (cursor.hot_x, cursor.hot_y))
}

fn power_menu_buffer() -> MemoryRenderBuffer {
    let pixels = shell::paint_power_menu();
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Argb8888,
        (shell::MENU_W, shell::MENU_H),
        1,
        Transform::Normal,
        None,
    )
}

fn configure_pointer_scroll(device: &mut InputDevice) {
    let methods = device.config_scroll_methods();
    let Some(chosen) = crate::input::preferred_scroll(&methods) else {
        return;
    };
    if device.config_scroll_method() != Some(chosen) {
        if let Err(err) = device.config_scroll_set_method(chosen) {
            tracing::warn!(name = device.name(), ?err, "scroll method");
            return;
        }
    }
    tracing::info!(name = device.name(), ?chosen, ?methods, "scroll method");
}

fn strip_buffer(
    height: i32,
    expanded: bool,
    page: Page,
    network_open: bool,
    power_open: bool,
    voice_live: bool,
) -> MemoryRenderBuffer {
    let width = shell::strip_width(expanded);
    let pixels = shell::paint_strip(width, height, expanded, page, network_open, power_open, voice_live);
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
