// Copyright 2019-2021 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use tauri::utils::Theme;
use tauri_runtime::{window::WindowEvent, Error, RunEvent, UserEvent};
#[cfg(target_os = "linux")]
use tauri_runtime_wry::wry::application::platform::unix::WindowExtUnix;
#[cfg(windows)]
use tauri_runtime_wry::wry::application::platform::windows::WindowExtWindows;
use tauri_runtime_wry::{
  center_window,
  wry::application::{
    event::{Event, WindowEvent as TaoWindowEvent},
    event_loop::{ControlFlow, EventLoopProxy, EventLoopWindowTarget},
    menu::CustomMenuItem,
    window::Fullscreen,
  },
  CursorIconWrapper, EventLoopIterationContext, HasRawWindowHandle, MenuEventListeners, Message,
  PhysicalPositionWrapper, PhysicalSizeWrapper, Plugin, PositionWrapper, RawWindowHandle,
  SizeWrapper, WebContextStore, WebviewId, WindowEventListeners, WindowEventWrapper,
  WindowMenuEventListeners, WindowMessage,
};

#[cfg(target_os = "linux")]
use glutin::platform::ContextTraitExt;
#[cfg(target_os = "linux")]
use gtk::prelude::*;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU8, Ordering};

use std::{
  cell::RefCell,
  collections::HashMap,
  ops::Deref,
  rc::Rc,
  sync::{
    mpsc::{channel, Receiver, SyncSender},
    Arc, Mutex,
  },
};

pub mod window;
use window::Window;

pub struct CreateWindowPayload {
  window_id: WebviewId,
  label: String,
  app: Box<dyn epi::App + Send>,
  native_options: epi::NativeOptions,
}

pub struct EguiPlugin<T: UserEvent> {
  pub(crate) proxy: EventLoopProxy<Message<T>>,
  pub(crate) create_window_channel: (
    SyncSender<CreateWindowPayload>,
    Receiver<CreateWindowPayload>,
  ),
  pub(crate) windows: Arc<Mutex<HashMap<WebviewId, WindowWrapper>>>,
  pub(crate) is_focused: bool,
}

pub struct EguiPluginHandle<T: UserEvent = tauri::EventLoopMessage> {
  proxy: EventLoopProxy<Message<T>>,
  create_window_tx: SyncSender<CreateWindowPayload>,
}

impl<T: UserEvent> EguiPlugin<T> {
  pub(crate) fn handle(&self) -> EguiPluginHandle<T> {
    EguiPluginHandle {
      proxy: self.proxy.clone(),
      create_window_tx: self.create_window_channel.0.clone(),
    }
  }
}

impl<T: UserEvent> EguiPluginHandle<T> {
  pub fn create_window(
    &self,
    label: String,
    app: Box<dyn epi::App + Send>,
    native_options: epi::NativeOptions,
  ) -> tauri::Result<Window<T>> {
    let window_id = rand::random();
    let _ = self.create_window_tx.send(CreateWindowPayload {
      window_id,
      label,
      app,
      native_options,
    });
    // force the event loop to receive a new event
    let _ = self.proxy.send_event(Message::Task(Box::new(move || {})));
    Ok(Window {
      id: window_id,
      proxy: self.proxy.clone(),
    })
  }
}

impl<T: UserEvent> Plugin<T> for EguiPlugin<T> {
  #[allow(dead_code)]
  fn on_event(
    &mut self,
    event: &Event<Message<T>>,
    event_loop: &EventLoopWindowTarget<Message<T>>,
    proxy: &EventLoopProxy<Message<T>>,
    control_flow: &mut ControlFlow,
    context: EventLoopIterationContext<'_, T>,
    web_context: &WebContextStore,
  ) -> bool {
    if let Ok(payload) = self.create_window_channel.1.try_recv() {
      create_gl_window(
        event_loop,
        &context,
        &self.windows,
        payload.label,
        payload.app,
        payload.native_options,
        payload.window_id,
        proxy,
      );
    }
    handle_gl_loop(
      &self.windows,
      event,
      event_loop,
      control_flow,
      context,
      web_context,
      &mut self.is_focused,
    )
  }
}

#[allow(dead_code)]
pub enum MaybeRc<T> {
  Actual(T),
  Rc(Rc<T>),
}

impl<T> MaybeRc<T> {
  #[allow(dead_code)]
  pub fn new(t: T) -> Self {
    Self::Actual(t)
  }
}

impl<T> AsRef<T> for MaybeRc<T> {
  fn as_ref(&self) -> &T {
    match self {
      Self::Actual(t) => t,
      Self::Rc(t) => t,
    }
  }
}

impl<T> Deref for MaybeRc<T> {
  type Target = T;

  fn deref(&self) -> &Self::Target {
    match self {
      Self::Actual(v) => v,
      Self::Rc(r) => r.deref(),
    }
  }
}

impl<T> std::borrow::Borrow<T> for MaybeRc<T> {
  fn borrow(&self) -> &T {
    match self {
      Self::Actual(v) => v,
      Self::Rc(r) => r.borrow(),
    }
  }
}

#[allow(dead_code)]
pub enum MaybeRcCell<T> {
  Actual(RefCell<T>),
  RcCell(Rc<RefCell<T>>),
}

impl<T> MaybeRcCell<T> {
  #[allow(dead_code)]
  pub fn new(t: T) -> Self {
    Self::Actual(RefCell::new(t))
  }
}

impl<T> Deref for MaybeRcCell<T> {
  type Target = RefCell<T>;

  fn deref(&self) -> &Self::Target {
    match self {
      Self::Actual(v) => v,
      Self::RcCell(r) => r.deref(),
    }
  }
}

pub struct GlutinWindowContext {
  pub context: MaybeRc<glutin::ContextWrapper<glutin::PossiblyCurrent, glutin::window::Window>>,
  glow_context: MaybeRc<glow::Context>,
  painter: MaybeRcCell<egui_glow::Painter>,
  integration: MaybeRcCell<egui_tao::epi::EpiIntegration>,
  #[cfg(target_os = "linux")]
  render_flow: Rc<AtomicU8>,
}

#[allow(dead_code)]
pub struct WindowWrapper {
  label: String,
  inner: Option<Box<GlutinWindowContext>>,
  menu_items: Option<HashMap<u16, CustomMenuItem>>,
}

#[allow(clippy::too_many_arguments)]
pub fn create_gl_window<T: UserEvent>(
  event_loop: &EventLoopWindowTarget<Message<T>>,
  context: &EventLoopIterationContext<'_, T>,
  windows: &Arc<Mutex<HashMap<WebviewId, WindowWrapper>>>,
  label: String,
  app: Box<dyn epi::App + Send>,
  native_options: epi::NativeOptions,
  window_id: WebviewId,
  proxy: &EventLoopProxy<Message<T>>,
) {
  let persistence = egui_tao::epi::Persistence::from_app_name(app.name());
  let window_builder = egui_tao::epi::window_builder(&native_options, &None).with_title(app.name());
  let gl_window = unsafe {
    glutin::ContextBuilder::new()
      .with_depth_buffer(0)
      .with_srgb(true)
      .with_stencil_buffer(0)
      .with_vsync(true)
      .build_windowed(window_builder, event_loop)
      .unwrap()
      .make_current()
      .unwrap()
  };

  context
    .webview_id_map
    .insert(gl_window.window().id(), window_id);

  let gl = unsafe { glow::Context::from_loader_function(|s| gl_window.get_proc_address(s)) };

  unsafe {
    use glow::HasContext as _;
    gl.enable(glow::FRAMEBUFFER_SRGB);
  }

  struct GlowRepaintSignal<T: UserEvent>(EventLoopProxy<Message<T>>, WebviewId);

  impl<T: UserEvent> epi::backend::RepaintSignal for GlowRepaintSignal<T> {
    fn request_repaint(&self) {
      let _ = self
        .0
        .send_event(Message::Window(self.1, WindowMessage::RequestRedraw));
    }
  }

  let repaint_signal = std::sync::Arc::new(GlowRepaintSignal(proxy.clone(), window_id));

  let painter = egui_glow::Painter::new(&gl, None, "")
    .map_err(|error| eprintln!("some OpenGL error occurred {}\n", error))
    .unwrap();

  let integration = egui_tao::epi::EpiIntegration::new(
    "egui_glow",
    painter.max_texture_side(),
    gl_window.window(),
    repaint_signal,
    persistence,
    app,
  );

  context
    .window_event_listeners
    .lock()
    .unwrap()
    .insert(window_id, Default::default());

  context
    .menu_event_listeners
    .lock()
    .unwrap()
    .insert(window_id, WindowMenuEventListeners::default());

  #[cfg(not(target_os = "linux"))]
  {
    windows.lock().expect("poisoned webview collection").insert(
      window_id,
      WindowWrapper {
        label,
        inner: Some(Box::new(GlutinWindowContext {
          context: MaybeRc::new(gl_window),
          glow_context: MaybeRc::new(gl),
          painter: MaybeRcCell::new(painter),
          integration: MaybeRcCell::new(integration),
        })),
        menu_items: Default::default(),
      },
    );
  }
  #[cfg(target_os = "linux")]
  {
    let area = unsafe { gl_window.raw_handle() };
    let integration = Rc::new(RefCell::new(integration));
    let painter = Rc::new(RefCell::new(painter));
    let render_flow = Rc::new(AtomicU8::new(1));
    let gl_window = Rc::new(gl_window);
    let gl = Rc::new(gl);

    let i = integration.clone();
    let p = painter.clone();
    let r = render_flow.clone();
    let gl_window_ = Rc::downgrade(&gl_window);
    let gl_ = gl.clone();
    area.connect_render(move |_, _| {
      if let Some(gl_window) = gl_window_.upgrade() {
        let mut integration = i.borrow_mut();
        let mut painter = p.borrow_mut();
        let epi::egui::FullOutput {
          platform_output,
          needs_repaint,
          textures_delta,
          shapes,
        } = integration.update(gl_window.window());

        integration.handle_platform_output(gl_window.window(), platform_output);

        let clipped_meshes = integration.egui_ctx.tessellate(shapes);

        {
          let color = integration.app.clear_color();
          unsafe {
            use glow::HasContext as _;
            gl_.disable(glow::SCISSOR_TEST);
            gl_.clear_color(color[0], color[1], color[2], color[3]);
            gl_.clear(glow::COLOR_BUFFER_BIT);
          }
          painter.paint_and_update_textures(
            &gl_,
            gl_window.window().inner_size().into(),
            integration.egui_ctx.pixels_per_point(),
            clipped_meshes,
            &textures_delta,
          );
        }

        {
          let control_flow = if integration.should_quit() {
            1
          } else if needs_repaint {
            0
          } else {
            1
          };
          r.store(control_flow, Ordering::Relaxed);
        }

        integration.maybe_autosave(gl_window.window());
      }
      gtk::Inhibit(false)
    });

    windows.lock().expect("poisoned webview collection").insert(
      window_id,
      WindowWrapper {
        label,
        inner: Some(Box::new(GlutinWindowContext {
          context: MaybeRc::Rc(gl_window),
          glow_context: MaybeRc::Rc(gl),
          painter: MaybeRcCell::RcCell(painter),
          integration: MaybeRcCell::RcCell(integration),
          render_flow,
        })),
        menu_items: Default::default(),
      },
    );
  }
}

#[cfg(not(target_os = "linux"))]
fn win_mac_gl_loop<T: UserEvent>(
  control_flow: &mut ControlFlow,
  glutin_window_context: &mut GlutinWindowContext,
  event: &Event<Message<T>>,
  is_focused: bool,
  should_quit: &mut bool,
) {
  let mut redraw = || {
    let gl_window = &glutin_window_context.context;
    let gl = &glutin_window_context.glow_context;
    let mut integration = glutin_window_context.integration.borrow_mut();
    let mut painter = glutin_window_context.painter.borrow_mut();

    if !is_focused {
      // On Mac, a minimized Window uses up all CPU: https://github.com/emilk/egui/issues/325
      // We can't know if we are minimized: https://github.com/rust-windowing/winit/issues/208
      // But we know if we are focused (in foreground). When minimized, we are not focused.
      // However, a user may want an egui with an animation in the background,
      // so we still need to repaint quite fast.
      std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let epi::egui::FullOutput {
      platform_output,
      needs_repaint,
      textures_delta,
      shapes,
    } = integration.update(gl_window.window());

    integration.handle_platform_output(gl_window.window(), platform_output);

    let clipped_meshes = integration.egui_ctx.tessellate(shapes);

    {
      let color = integration.app.clear_color();
      unsafe {
        use glow::HasContext as _;
        gl.disable(glow::SCISSOR_TEST);
        gl.clear_color(color[0], color[1], color[2], color[3]);
        gl.clear(glow::COLOR_BUFFER_BIT);
      }
      painter.paint_and_update_textures(
        &gl,
        gl_window.window().inner_size().into(),
        integration.egui_ctx.pixels_per_point(),
        clipped_meshes,
        &textures_delta,
      );

      gl_window.swap_buffers().unwrap();
    }

    {
      *control_flow = if integration.should_quit() {
        *should_quit = true;
        ControlFlow::Wait
      } else if needs_repaint {
        gl_window.window().request_redraw();
        ControlFlow::Poll
      } else {
        ControlFlow::Wait
      };
    }

    integration.maybe_autosave(gl_window.window());
  };

  match event {
    Event::RedrawEventsCleared if cfg!(windows) => redraw(),
    Event::RedrawRequested(_) if !cfg!(windows) => redraw(),
    _ => (),
  }
}

#[cfg(target_os = "linux")]
fn linux_gl_loop<T: UserEvent>(
  control_flow: &mut ControlFlow,
  glutin_window_context: &mut GlutinWindowContext,
  event: &Event<Message<T>>,
) {
  let area = unsafe { glutin_window_context.context.raw_handle() };
  if let Event::MainEventsCleared = event {
    area.queue_render();
    match glutin_window_context.render_flow.load(Ordering::Relaxed) {
      0 => *control_flow = ControlFlow::Poll,
      1 => *control_flow = ControlFlow::Wait,
      2 => *control_flow = ControlFlow::Exit,
      _ => unreachable!(),
    }
  }
}

pub fn handle_gl_loop<T: UserEvent>(
  windows: &Arc<Mutex<HashMap<WebviewId, WindowWrapper>>>,
  event: &Event<'_, Message<T>>,
  _event_loop: &EventLoopWindowTarget<Message<T>>,
  control_flow: &mut ControlFlow,
  context: EventLoopIterationContext<'_, T>,
  _web_context: &WebContextStore,
  is_focused: &mut bool,
) -> bool {
  let mut prevent_default = false;
  let EventLoopIterationContext {
    callback,
    window_event_listeners,
    menu_event_listeners,
    webview_id_map,
    ..
  } = context;
  let has_egui_window = !windows.lock().unwrap().is_empty();
  if has_egui_window {
    let mut windows_lock = windows.lock().unwrap();

    let iter = windows_lock.values_mut();
    for win in iter {
      if let Some(glutin_window_context) = &mut win.inner {
        #[cfg(not(target_os = "linux"))]
        win_mac_gl_loop(
          control_flow,
          glutin_window_context,
          &event,
          *is_focused,
          &mut should_quit,
        );
        #[cfg(target_os = "linux")]
        linux_gl_loop(control_flow, glutin_window_context, event);
      }
    }

    match event {
      Event::WindowEvent {
        event, window_id, ..
      } => {
        let window_id = webview_id_map.get(window_id);
        if let Some((label, glutin_window_context)) = windows_lock
          .get_mut(&window_id)
          .map(|win| (&win.label, &mut win.inner))
        {
          match event {
            TaoWindowEvent::Focused(new_focused) => {
              *is_focused = *new_focused;
            }
            TaoWindowEvent::Resized(physical_size) => {
              if let Some(glutin_window_context) = glutin_window_context.as_ref() {
                glutin_window_context.context.resize(*physical_size);
              }
            }
            _ => (),
          }

          if let TaoWindowEvent::CloseRequested = event {
            on_close_requested(
              callback,
              window_id,
              (label, glutin_window_context),
              window_event_listeners,
              menu_event_listeners.clone(),
            );
          }

          let mut should_quit = false;
          if let Some(glutin_window_context) = glutin_window_context.as_ref() {
            let gl_window = &glutin_window_context.context;
            let mut integration = glutin_window_context.integration.borrow_mut();
            integration.on_event(event);
            if integration.should_quit() {
              should_quit = true;
              *control_flow = ControlFlow::Wait;
            }
            gl_window.window().request_redraw();
          }
          if should_quit {
            *glutin_window_context = None;
            menu_event_listeners.lock().unwrap().remove(&window_id);
          }

          {
            if let Some(label) = windows_lock.get(&window_id).map(|w| &w.label) {
              if let Some(event) = WindowEventWrapper::from(event).0 {
                let label = label.clone();
                drop(windows_lock);
                callback(RunEvent::WindowEvent {
                  label,
                  event: event.clone(),
                });
                let shared_listeners = context
                  .window_event_listeners
                  .lock()
                  .unwrap()
                  .get(&window_id)
                  .unwrap()
                  .clone();
                let listeners = shared_listeners.lock().unwrap();
                let handlers = listeners.values();
                for handler in handlers {
                  handler(&event);
                }
              }
            }
          }
          prevent_default = true;
        }
      }

      Event::UserEvent(Message::Window(window_id, message)) => {
        if let Some(Some(glutin_window_context)) =
          windows_lock.get_mut(window_id).map(|win| &mut win.inner)
        {
          let window = glutin_window_context.context.window();
          match message {
            WindowMessage::ScaleFactor(tx) => tx.send(window.scale_factor()).unwrap(),
            WindowMessage::InnerPosition(tx) => tx
              .send(
                window
                  .inner_position()
                  .map(|p| PhysicalPositionWrapper(p).into())
                  .map_err(|_| Error::FailedToSendMessage),
              )
              .unwrap(),
            WindowMessage::OuterPosition(tx) => tx
              .send(
                window
                  .outer_position()
                  .map(|p| PhysicalPositionWrapper(p).into())
                  .map_err(|_| Error::FailedToSendMessage),
              )
              .unwrap(),
            WindowMessage::InnerSize(tx) => tx
              .send(PhysicalSizeWrapper(window.inner_size()).into())
              .unwrap(),
            WindowMessage::OuterSize(tx) => tx
              .send(PhysicalSizeWrapper(window.outer_size()).into())
              .unwrap(),
            WindowMessage::IsFullscreen(tx) => tx.send(window.fullscreen().is_some()).unwrap(),
            WindowMessage::IsMaximized(tx) => tx.send(window.is_maximized()).unwrap(),
            WindowMessage::IsDecorated(tx) => tx.send(window.is_decorated()).unwrap(),
            WindowMessage::IsResizable(tx) => tx.send(window.is_resizable()).unwrap(),
            WindowMessage::IsVisible(tx) => tx.send(window.is_visible()).unwrap(),
            WindowMessage::IsMenuVisible(tx) => tx.send(window.is_menu_visible()).unwrap(),
            WindowMessage::CurrentMonitor(tx) => tx.send(window.current_monitor()).unwrap(),
            WindowMessage::PrimaryMonitor(tx) => tx.send(window.primary_monitor()).unwrap(),
            WindowMessage::AvailableMonitors(tx) => {
              tx.send(window.available_monitors().collect()).unwrap()
            }
            WindowMessage::RawWindowHandle(tx) => tx
              .send(RawWindowHandle(window.raw_window_handle()))
              .unwrap(),
            WindowMessage::Theme(tx) => {
              #[cfg(any(windows, target_os = "macos"))]
              tx.send(tauri_runtime_wry::map_theme(&window.theme()))
                .unwrap();
              #[cfg(not(windows))]
              tx.send(Theme::Light).unwrap();
            }
            // Setters
            WindowMessage::Center => {
              let _ = center_window(window, window.inner_size());
            }
            WindowMessage::RequestUserAttention(request_type) => {
              window.request_user_attention(request_type.as_ref().map(|r| r.0));
            }
            WindowMessage::SetResizable(resizable) => window.set_resizable(*resizable),
            WindowMessage::SetTitle(title) => window.set_title(title),
            WindowMessage::Maximize => window.set_maximized(true),
            WindowMessage::Unmaximize => window.set_maximized(false),
            WindowMessage::Minimize => window.set_minimized(true),
            WindowMessage::Unminimize => window.set_minimized(false),
            WindowMessage::ShowMenu => window.show_menu(),
            WindowMessage::HideMenu => window.hide_menu(),
            WindowMessage::Show => window.set_visible(true),
            WindowMessage::Hide => window.set_visible(false),
            WindowMessage::Close => {
              // TODO
              /*on_window_close(
                *window_id,
                glutin_window_context,
                menu_event_listeners.clone(),
              );*/
            }
            WindowMessage::SetDecorations(decorations) => window.set_decorations(*decorations),
            WindowMessage::SetAlwaysOnTop(always_on_top) => {
              window.set_always_on_top(*always_on_top);
            }
            WindowMessage::SetSize(size) => {
              window.set_inner_size(SizeWrapper::from(*size).0);
            }
            WindowMessage::SetMinSize(size) => {
              window.set_min_inner_size(size.map(|s| SizeWrapper::from(s).0));
            }
            WindowMessage::SetMaxSize(size) => {
              window.set_max_inner_size(size.map(|s| SizeWrapper::from(s).0));
            }
            WindowMessage::SetPosition(position) => {
              window.set_outer_position(PositionWrapper::from(*position).0)
            }
            WindowMessage::SetFullscreen(fullscreen) => {
              if *fullscreen {
                window.set_fullscreen(Some(Fullscreen::Borderless(None)))
              } else {
                window.set_fullscreen(None)
              }
            }
            WindowMessage::SetFocus => {
              window.set_focus();
            }
            WindowMessage::SetIcon(icon) => {
              window.set_window_icon(Some(icon.clone()));
            }
            #[allow(unused_variables)]
            WindowMessage::SetSkipTaskbar(skip) => {
              #[cfg(any(windows, target_os = "linux"))]
              window.set_skip_taskbar(*skip);
            }
            WindowMessage::SetCursorGrab(grab) => {
              let _ = window.set_cursor_grab(*grab);
            }
            WindowMessage::SetCursorVisible(visible) => {
              window.set_cursor_visible(*visible);
            }
            WindowMessage::SetCursorIcon(icon) => {
              window.set_cursor_icon(CursorIconWrapper::from(*icon).0);
            }
            WindowMessage::SetCursorPosition(position) => {
              let _ = window.set_cursor_position(PositionWrapper::from(*position).0);
            }
            WindowMessage::DragWindow => {
              let _ = window.drag_window();
            }
            WindowMessage::UpdateMenuItem(_id, _update) => {
              // TODO
            }
            WindowMessage::RequestRedraw => {
              window.request_redraw();
            }
            _ => (),
          }
        }
      }

      _ => (),
    }
  }

  prevent_default
}

fn on_close_requested<'a, T: UserEvent>(
  callback: &'a mut (dyn FnMut(RunEvent<T>) + 'static),
  window_id: WebviewId,
  (label, glutin_window_context): (&str, &mut Option<Box<GlutinWindowContext>>),
  window_event_listeners: &WindowEventListeners,
  menu_event_listeners: MenuEventListeners,
) {
  let (tx, rx) = channel();
  let shared_listeners = window_event_listeners
    .lock()
    .unwrap()
    .get(&window_id)
    .unwrap()
    .clone();
  let listeners = shared_listeners.lock().unwrap();
  let handlers = listeners.values();
  for handler in handlers {
    handler(&WindowEvent::CloseRequested {
      signal_tx: tx.clone(),
    });
  }
  callback(RunEvent::WindowEvent {
    label: label.into(),
    event: WindowEvent::CloseRequested { signal_tx: tx },
  });
  if let Ok(true) = rx.try_recv() {
  } else {
    on_window_close(window_id, glutin_window_context, menu_event_listeners);
  }
}

fn on_window_close(
  window_id: WebviewId,
  glutin_window_context: &mut Option<Box<GlutinWindowContext>>,
  menu_event_listeners: MenuEventListeners,
) {
  // Destrooy GL context if its a GLWindow
  menu_event_listeners.lock().unwrap().remove(&window_id);
  if let Some(glutin_window_context) = glutin_window_context.take() {
    #[cfg(not(target_os = "linux"))]
    {
      glutin_window_context
        .integration
        .borrow_mut()
        .on_exit(glutin_window_context.context.window());
      glutin_window_context
        .painter
        .borrow_mut()
        .destroy(&glutin_window_context.glow_context);
    }
    #[cfg(target_os = "linux")]
    {
      let mut integration = glutin_window_context.integration.borrow_mut();
      integration.on_exit(glutin_window_context.context.window());
      glutin_window_context
        .painter
        .borrow_mut()
        .destroy(&glutin_window_context.glow_context);
    }
  }
}
