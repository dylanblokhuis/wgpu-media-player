use clipboard::{ClipboardContext, ClipboardProvider};
use winit::event::{VirtualKeyCode, WindowEvent};

#[derive(Default)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub mac_cmd: bool,
    pub command: bool,
}

#[derive(Default)]
pub struct Input {
    pub modifiers: Modifiers,
}

pub struct App {
    input: Input,
    on_load_file_request: Option<Box<dyn FnOnce(String)>>,
    clipboard: ClipboardContext,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: Input::default(),
            on_load_file_request: None,
            clipboard: ClipboardProvider::new().unwrap(),
        }
    }

    pub fn set_on_load_file_request<F: FnOnce(String) + Send + 'static>(&mut self, func: F) {
        self.on_load_file_request = Some(Box::new(func));
    }

    pub fn handle_window_event(&mut self, event: &WindowEvent) {
        fn format_url(url: &str) -> String {
            if url.starts_with("http") {
                url.to_string()
            } else if cfg!(target_os = "windows") {
                format!("file:///{}", url.replace('\\', "/"))
            } else {
                format!("file://{}", url)
            }
        }

        match event {
            WindowEvent::ModifiersChanged(state) => {
                self.input.modifiers.alt = state.alt();
                self.input.modifiers.ctrl = state.ctrl();
                self.input.modifiers.shift = state.shift();
                self.input.modifiers.mac_cmd = cfg!(target_os = "macos") && state.logo();
                self.input.modifiers.command = if cfg!(target_os = "macos") {
                    state.logo()
                } else {
                    state.ctrl()
                };
            }
            WindowEvent::KeyboardInput { input, .. } => {
                if let Some(keycode) = input.virtual_keycode {
                    if self.input.modifiers.command && keycode == VirtualKeyCode::V {
                        if let Ok(path_or_url) = self.clipboard.get_contents() {
                            if let Some(on_load_file_request) = self.on_load_file_request.take() {
                                on_load_file_request(format_url(&path_or_url));
                            }
                        }
                    }
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(on_load_file_request) = self.on_load_file_request.take() {
                    on_load_file_request(format_url(&path.to_string_lossy()));
                }
            }
            _ => {}
        }
    }
}
