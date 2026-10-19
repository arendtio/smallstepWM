use std::time::Duration;

use smithay::{
    backend::{
        allocator::{
            Format, Fourcc, Modifier,
            dmabuf::{AsDmabuf, Dmabuf},
            gbm::{GbmAllocator, GbmBuffer, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            gbm::{GbmFramebuffer, framebuffer_from_bo},
        },
        egl::{
            EGLContext, EGLDevice, EGLDisplay,
            context::{GlAttributes, PixelFormatRequirements},
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Bind, Color32F, ImportAll, ImportDma, ImportMem, Renderer,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
            pixman::PixmanRenderer,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev,
    },
    desktop::Window,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode as CalloopMode, PostAction, generic::Generic,
        },
        drm::{
            buffer::Buffer as DrmBuffer,
            control::{
                self, Device as ControlDevice, ModeTypeFlags, PageFlipFlags, connector, crtc,
                dumbbuffer, framebuffer,
            },
        },
        input::{Device as InputDevice, Libinput},
        pixman,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Logical, Point, Scale, Transform},
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

enum Engine {
    Soft {
        renderer: PixmanRenderer,
        buffers: Vec<ScanBuffer>,
    },
    Gl {
        renderer: GlesRenderer,
        buffers: Vec<GlScan>,
        // The buffer objects borrow the GBM device, so it has to outlive them.
        // Fields drop in declaration order: renderer, then buffers, then this.
        _allocator: GbmAllocator<DrmDeviceFd>,
    },
}

struct GlScan {
    handle: framebuffer::Handle,
    // Dropped before the buffer object. The handle above is only a copy.
    _fb: GbmFramebuffer,
    dmabuf: Dmabuf,
    _bo: GbmBuffer,
}

pub struct Gpu {
    engine: Engine,
    pub drm: DrmDevice,
    pub fd: DrmDeviceFd,
    pub damage: OutputDamageTracker,
    pub background: MemoryRenderBuffer,
    pub strip: MemoryRenderBuffer,
    pub strip_key: (bool, Page, bool, bool, bool, i32, i32, u64),
    pub power_menu: MemoryRenderBuffer,
    pub cursor: MemoryRenderBuffer,
    pub cursor_hot: (i32, i32),
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub mode: control::Mode,
    pub displayed: Option<usize>,
    pub pending: Option<usize>,
}

impl Gpu {
    pub(crate) fn import_client_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let Engine::Gl { renderer, .. } = &mut self.engine else {
            return false;
        };
        renderer.import_dmabuf(dmabuf, None).is_ok()
    }

    fn is_accelerated(&self) -> bool {
        matches!(self.engine, Engine::Gl { .. })
    }

    fn dmabuf_formats(&self) -> Vec<Format> {
        let Engine::Gl { renderer, .. } = &self.engine else {
            return Vec::new();
        };
        renderer.dmabuf_formats().iter().copied().collect()
    }

    fn buffer_count(&self) -> usize {
        match &self.engine {
            Engine::Soft { buffers, .. } => buffers.len(),
            Engine::Gl { buffers, .. } => buffers.len(),
        }
    }

    fn framebuffer(&self, index: usize) -> framebuffer::Handle {
        match &self.engine {
            Engine::Soft { buffers, .. } => buffers[index].fb,
            Engine::Gl { buffers, .. } => buffers[index].handle,
        }
    }

    fn replace_with_software(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (width, height) = self.mode.size();
        self.engine = software_engine(&self.fd, (u32::from(width), u32::from(height)))?;
        self.displayed = None;
        self.pending = None;
        Ok(())
    }
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
    let (pages_tx, pages_channel) = smithay::reexports::calloop::channel::channel();
    state.pages_tx = Some(pages_tx);
    event_loop
        .handle()
        .insert_source(pages_channel, |event, _, state| {
            if let smithay::reexports::calloop::channel::Event::Msg(report) = event {
                state.apply_pages(report);
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

    let accelerated = state.gpu.as_ref().is_some_and(|gpu| gpu.is_accelerated());
    if let Err(err) = pump(&mut state) {
        if !accelerated {
            return Err(err);
        }
        tracing::warn!(%err, "OpenGL ES frame failed, using software rendering");
        fallback_to_software(&mut state)?;
        pump(&mut state)?;
    } else if accelerated
        && state
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.displayed.is_none())
    {
        tracing::warn!("OpenGL ES did not show a frame, using software rendering");
        fallback_to_software(&mut state)?;
        pump(&mut state)?;
    }
    if state
        .gpu
        .as_ref()
        .is_some_and(|gpu| gpu.displayed.is_none())
    {
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

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| "failed to assign the libinput seat")?;
    let libinput_saved = libinput.clone();
    handle
        .insert_source(
            LibinputInputBackend::new(libinput),
            |event, _, state| match event {
                smithay::backend::input::InputEvent::DeviceAdded { mut device } => {
                    configure_pointer_scroll(&mut device);
                }
                other => state.on_input(other),
            },
        )
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
        .open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
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
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output_size = mode.size.to_logical(1);
    state.output = Some(output.clone());

    let (width, height) = picked.mode.size();
    let size = (u32::from(width), u32::from(height));
    let engine = if hardware_requested(std::env::var("SMALLSTEP_RENDERER").ok().as_deref()) {
        match try_accelerated(&fd, size) {
            Ok(engine) => engine,
            Err(err) => {
                tracing::warn!(reason = %err, "OpenGL ES unavailable, using software rendering");
                software_engine(&fd, size)?
            }
        }
    } else {
        tracing::warn!("using software rendering");
        software_engine(&fd, size)?
    };
    let damage = OutputDamageTracker::from_output(&output);
    let background = background_buffer(state.output_size.w, state.output_size.h);
    let strip = strip_buffer(
        state.output_size.h,
        false,
        Page::Start,
        false,
        false,
        false,
        &[],
        0,
    );
    let (cursor, cursor_hot) = cursor_buffer();
    let gpu = Gpu {
        engine,
        drm,
        fd,
        damage,
        background,
        strip,
        strip_key: (
            false,
            Page::Start,
            false,
            false,
            false,
            state.output_size.w,
            state.output_size.h,
            shell::strip_stamp(Page::Start, &[], 0),
        ),
        power_menu: power_menu_buffer(),
        cursor,
        cursor_hot,
        crtc: picked.crtc,
        connector: picked.connector,
        mode: picked.mode,
        displayed: None,
        pending: None,
    };
    if gpu.is_accelerated() {
        let formats = gpu.dmabuf_formats();
        let count = formats.len();
        if count > 0 {
            state.dmabuf_global = Some(
                state
                    .dmabuf_state
                    .create_global::<State>(&state.display_handle, formats),
            );
        }
        tracing::warn!(dmabuf_formats = count, "using OpenGL ES");
    }
    Ok(gpu)
}

fn alloc_scan_buffers(
    fd: &DrmDeviceFd,
    size: (u32, u32),
) -> Result<Vec<ScanBuffer>, Box<dyn std::error::Error>> {
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
            window.send_frame(&output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
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
    let hud_text = if !state.voice.note.is_empty() {
        Some(state.voice.note.clone())
    } else if !state.pages_note.is_empty() {
        Some(state.pages_note.clone())
    } else if !state.power_note.is_empty() {
        Some(state.power_note.clone())
    } else {
        None
    };
    let voice_note = hud_text.as_deref().map(shell::paint_voice_note);
    let labels: Vec<String> = state
        .pages
        .iter()
        .map(|page| crate::pages::label(&page.title, &page.url))
        .collect();
    let page_rows: Vec<shell::StripPage<'_>> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| shell::StripPage {
            label,
            active: page == Page::Browser
                && state.active_context.as_deref() == Some(state.pages[index].context.as_str()),
        })
        .collect();
    let page_scroll = state.page_scroll;
    let strip_stamp = shell::strip_stamp(page, &page_rows, page_scroll);
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
        let password = state
            .wifi
            .password_for
            .as_ref()
            .map(|_| state.wifi.password.as_str());
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
    if gpu.strip_key
        != (
            expanded,
            page,
            network_open,
            power_menu,
            voice_live,
            size.w,
            size.h,
            strip_stamp,
        )
    {
        gpu.strip = strip_buffer(
            size.h,
            expanded,
            page,
            network_open,
            power_menu,
            voice_live,
            &page_rows,
            page_scroll,
        );
        gpu.strip_key = (
            expanded,
            page,
            network_open,
            power_menu,
            voice_live,
            size.w,
            size.h,
            strip_stamp,
        );
    }

    let decor = Decor {
        pointer,
        page,
        power_open: power_menu,
        size_w: size.w,
        size_h: size.h,
        startup,
        voice_note,
        wifi_pixels,
        windows,
    };
    let index = gpu
        .displayed
        .map(|shown| (shown + 1) % gpu.buffer_count())
        .unwrap_or(0);
    // Front to back: cursor, strip, Wi-Fi panel, power menu, startup bar, window, then the gradient.
    render_into(gpu, index, &decor)?;
    present(gpu, index)
}

fn render_into(
    gpu: &mut Gpu,
    index: usize,
    decor: &Decor,
) -> Result<(), Box<dyn std::error::Error>> {
    let Gpu {
        engine,
        damage,
        cursor,
        cursor_hot,
        strip,
        background,
        power_menu,
        fd,
        ..
    } = gpu;
    match engine {
        Engine::Soft { renderer, buffers } => {
            let elements = compose(
                renderer,
                cursor,
                *cursor_hot,
                strip,
                background,
                power_menu,
                decor,
            )?;
            paint_soft(renderer, damage, fd, &mut buffers[index], &elements)?;
        }
        Engine::Gl {
            renderer, buffers, ..
        } => {
            let elements = compose(
                renderer,
                cursor,
                *cursor_hot,
                strip,
                background,
                power_menu,
                decor,
            )?;
            paint_gl(renderer, damage, &buffers[index], &elements)?;
        }
    }
    Ok(())
}

struct Decor {
    pointer: Point<f64, Logical>,
    page: Page,
    power_open: bool,
    size_w: i32,
    size_h: i32,
    startup: Option<f32>,
    voice_note: Option<(Vec<u8>, i32, i32)>,
    wifi_pixels: Option<Vec<u8>>,
    windows: Vec<(Window, Point<i32, Logical>)>,
}

fn compose<R>(
    renderer: &mut R,
    cursor: &MemoryRenderBuffer,
    cursor_hot: (i32, i32),
    strip: &MemoryRenderBuffer,
    background: &MemoryRenderBuffer,
    power_menu: &MemoryRenderBuffer,
    decor: &Decor,
) -> Result<Vec<ShellElement<R>>, String>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: smithay::backend::renderer::Texture + Send + Clone + 'static,
    R::Error: std::error::Error,
{
    let mut elements: Vec<ShellElement<R>> = Vec::new();
    let cursor_at = (
        decor.pointer.x as i32 - cursor_hot.0,
        decor.pointer.y as i32 - cursor_hot.1,
    );
    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (f64::from(cursor_at.0), f64::from(cursor_at.1)),
            cursor,
            None,
            None,
            None,
            Kind::Cursor,
        )
        .map_err(|err| err.to_string())?,
    ));
    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            strip,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map_err(|err| err.to_string())?,
    ));
    if let Some((pixels, width, height)) = &decor.voice_note {
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
                renderer,
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
    if let Some(pixels) = &decor.wifi_pixels {
        let (x, y) = shell::wifi_origin(decor.size_h);
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
                renderer,
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
    if decor.power_open {
        let (x, y) = shell::power_menu_origin(decor.size_h);
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (f64::from(x), f64::from(y)),
                power_menu,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .map_err(|err| err.to_string())?,
        ));
    }
    if let Some(phase) = decor.startup {
        let pixels = shell::paint_startup_bar(phase);
        let bar = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (shell::BAR_W, shell::BAR_H),
            1,
            Transform::Normal,
            None,
        );
        let content_w = (decor.size_w - shell::STRIP_COLLAPSED).max(1);
        let x = shell::STRIP_COLLAPSED + (content_w - shell::BAR_W) / 2;
        let y = (decor.size_h - shell::BAR_H) / 2;
        elements.push(ShellElement::Memory(
            MemoryRenderBufferRenderElement::from_buffer(
                renderer,
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

    if decor.page == Page::Browser {
        if let Some((window, loc)) = decor.windows.last() {
            let at = Point::<i32, smithay::utils::Physical>::from((loc.x, loc.y));
            elements.extend(window.render_elements(renderer, at, Scale::from(1.0), 1.0));
        }
    }

    elements.push(ShellElement::Memory(
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            background,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .map_err(|err| err.to_string())?,
    ));
    Ok(elements)
}

fn paint_soft(
    renderer: &mut PixmanRenderer,
    damage: &mut OutputDamageTracker,
    fd: &DrmDeviceFd,
    buffer: &mut ScanBuffer,
    elements: &[ShellElement<PixmanRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = DrmBuffer::size(&buffer.dumb);
    let pitch = DrmBuffer::pitch(&buffer.dumb) as usize;
    let mut mapping = fd.map_dumb_buffer(&mut buffer.dumb).map_err(|err| {
        format!(
            "mapping the dumb buffer failed: {err} (os error {:?})",
            err.raw_os_error()
        )
    })?;
    let format =
        pixman::FormatCode::try_from(Fourcc::Xrgb8888).map_err(|_| "unsupported scanout format")?;
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
    let mut target = renderer.bind(&mut image).map_err(|err| err.to_string())?;
    damage
        .render_output(
            renderer,
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

fn paint_gl(
    renderer: &mut GlesRenderer,
    damage: &mut OutputDamageTracker,
    buffer: &GlScan,
    elements: &[ShellElement<GlesRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut dmabuf = buffer.dmabuf.clone();
    let mut target = renderer.bind(&mut dmabuf).map_err(|err| err.to_string())?;
    damage
        .render_output(
            renderer,
            &mut target,
            0,
            elements,
            Color32F::new(0.08, 0.09, 0.11, 1.0),
        )
        .map_err(|err| err.to_string())?;
    drop(target);
    // The scanout buffer is a dma-buf. Finish so the display does not read it mid-draw.
    renderer
        .with_context(|gl| unsafe { gl.Finish() })
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn present(gpu: &mut Gpu, index: usize) -> Result<bool, Box<dyn std::error::Error>> {
    let fb = gpu.framebuffer(index);
    if gpu.displayed.is_none() || gpu.buffer_count() == 1 {
        gpu.fd
            .set_crtc(gpu.crtc, Some(fb), (0, 0), &[gpu.connector], Some(gpu.mode))
            .map_err(|err| {
                format!(
                    "setting the display mode failed: {err} (os error {:?})",
                    err.raw_os_error()
                )
            })?;
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
        Err(err) => Err(format!(
            "page flip failed: {err} (os error {:?})",
            err.raw_os_error()
        )
        .into()),
    }
}

fn restore_scanout(gpu: &mut Gpu) -> Result<(), String> {
    let Some(index) = gpu.displayed else {
        return Ok(());
    };
    let fb = gpu.framebuffer(index);
    gpu.fd
        .set_crtc(gpu.crtc, Some(fb), (0, 0), &[gpu.connector], Some(gpu.mode))
        .map_err(|err| {
            format!(
                "setting the display mode failed: {err} (os error {:?})",
                err.raw_os_error()
            )
        })
}

fn fallback_to_software(state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(global) = state.dmabuf_global.take() {
        state
            .dmabuf_state
            .disable_global::<State>(&state.display_handle, &global);
        state
            .dmabuf_state
            .destroy_global::<State>(&state.display_handle, global);
    }
    let gpu = state.gpu.as_mut().ok_or("no gpu")?;
    gpu.replace_with_software()?;
    state.dirty = true;
    state.frame_pending = false;
    Ok(())
}

fn software_engine(
    fd: &DrmDeviceFd,
    size: (u32, u32),
) -> Result<Engine, Box<dyn std::error::Error>> {
    let renderer = PixmanRenderer::new().map_err(|err| err.to_string())?;
    let buffers = alloc_scan_buffers(fd, size)?;
    Ok(Engine::Soft { renderer, buffers })
}

fn hardware_requested(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "software" | "soft" | "pixman"
    )
}

fn same_gpu(seat_major: u32, seat_minor: u32, device_major: u32, device_minor: u32) -> bool {
    seat_major == device_major && seat_minor % 64 == device_minor % 64
}

fn try_accelerated(fd: &DrmDeviceFd, size: (u32, u32)) -> Result<Engine, String> {
    let seat = DrmNode::from_file(fd).map_err(|err| err.to_string())?;
    let device = match_hardware_device(seat)?;
    // The EGL device is a process-wide handle. Nothing else has made it current.
    let display = unsafe { EGLDisplay::new(device) }.map_err(|err| err.to_string())?;
    let context = egl_context(&display)?;
    // This thread is still the only one, and the context is not current elsewhere.
    let mut renderer = unsafe { GlesRenderer::new(context) }.map_err(|err| err.to_string())?;
    let gbm = GbmDevice::new(fd.clone()).map_err(|err| err.to_string())?;
    let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let mut last = String::from("no scanout format");
    for fourcc in [Fourcc::Argb8888, Fourcc::Xrgb8888] {
        let buffers = match alloc_gl_buffers_format(&mut allocator, fd, size, fourcc) {
            Ok(buffers) => buffers,
            Err(err) => {
                last = err;
                continue;
            }
        };
        let mut dmabuf = buffers[0].dmabuf.clone();
        match renderer.bind(&mut dmabuf) {
            Ok(target) => {
                drop(target);
                return Ok(Engine::Gl {
                    renderer,
                    buffers,
                    _allocator: allocator,
                });
            }
            Err(err) => last = err.to_string(),
        }
    }
    Err(last)
}

fn match_hardware_device(seat: DrmNode) -> Result<EGLDevice, String> {
    let devices = EGLDevice::enumerate().map_err(|err| err.to_string())?;
    for device in devices {
        if device.is_software() {
            continue;
        }
        let Ok(Some(node)) = device.try_get_render_node() else {
            continue;
        };
        if same_gpu(seat.major(), seat.minor(), node.major(), node.minor()) {
            return Ok(device);
        }
    }
    Err("no hardware EGL device for this seat".into())
}

fn egl_context(display: &EGLDisplay) -> Result<EGLContext, String> {
    let reqs = PixelFormatRequirements {
        hardware_accelerated: Some(true),
        color_bits: Some(24),
        float_color_buffer: false,
        alpha_bits: None,
        depth_bits: None,
        stencil_bits: None,
        multisampling: None,
    };
    let mut last = String::from("no GLES context");
    for major in [3, 2] {
        let attributes = GlAttributes {
            version: (major, 0),
            profile: None,
            debug: false,
            vsync: false,
        };
        match EGLContext::new_with_config(display, attributes, reqs) {
            Ok(context) => return Ok(context),
            Err(err) => last = err.to_string(),
        }
    }
    Err(last)
}

fn alloc_gl_buffers_format(
    allocator: &mut GbmAllocator<DrmDeviceFd>,
    fd: &DrmDeviceFd,
    size: (u32, u32),
    fourcc: Fourcc,
) -> Result<Vec<GlScan>, String> {
    let modifiers = [Modifier::Linear, Modifier::Invalid];
    let flags = GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT;
    let mut buffers = Vec::new();
    for index in 0..2 {
        let bo = match allocator.create_buffer_with_flags(size.0, size.1, fourcc, &modifiers, flags)
        {
            Ok(bo) => bo,
            Err(err) if index == 0 => return Err(format!("GBM buffer: {err}")),
            Err(err) => {
                tracing::warn!(%err, "second GLES scanout buffer unavailable");
                break;
            }
        };
        let dmabuf = match bo.export() {
            Ok(dmabuf) => dmabuf,
            Err(err) if index == 0 => {
                return Err(format!("exporting the GBM buffer failed: {err}"));
            }
            Err(err) => {
                tracing::warn!(%err, "second GLES scanout buffer unavailable");
                break;
            }
        };
        let fb = match framebuffer_from_bo(fd, &bo, true) {
            Ok(fb) => fb,
            Err(err) if index == 0 => return Err(format!("GBM framebuffer: {err}")),
            Err(err) => {
                tracing::warn!(%err, "second GLES scanout buffer unavailable");
                break;
            }
        };
        let handle = *fb.as_ref();
        buffers.push(GlScan {
            handle,
            _fb: fb,
            dmabuf,
            _bo: bo,
        });
    }
    if buffers.is_empty() {
        return Err("no GLES scanout buffer".into());
    }
    Ok(buffers)
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
    pages: &[shell::StripPage<'_>],
    scroll: i32,
) -> MemoryRenderBuffer {
    let width = shell::strip_width(expanded);
    let pixels = shell::paint_strip(
        width,
        height,
        expanded,
        page,
        network_open,
        power_open,
        voice_live,
        pages,
        scroll,
    );
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
        if let Some(crtc) = resources
            .filter_crtcs(encoder.possible_crtcs())
            .into_iter()
            .next()
        {
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
        refresh: if refresh == 0 {
            60_000
        } else {
            (refresh as i32) * 1000
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{hardware_requested, same_gpu};

    #[test]
    fn software_names_force_the_cpu_path() {
        assert!(!hardware_requested(Some("software")));
        assert!(!hardware_requested(Some("soft")));
        assert!(!hardware_requested(Some("pixman")));
        assert!(!hardware_requested(Some(" Software ")));
    }

    #[test]
    fn other_names_try_hardware() {
        assert!(hardware_requested(None));
        assert!(hardware_requested(Some("")));
        assert!(hardware_requested(Some("auto")));
        assert!(hardware_requested(Some("gl")));
        assert!(hardware_requested(Some("egl")));
    }

    #[test]
    fn primary_and_render_node_are_the_same_gpu() {
        assert!(same_gpu(226, 0, 226, 128));
        assert!(same_gpu(226, 1, 226, 129));
        assert!(!same_gpu(226, 0, 226, 129));
        assert!(!same_gpu(226, 0, 226, 1));
    }
}
