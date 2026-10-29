use crate::event::Msg;
use rio_backend::event::WindowSize;
use std::borrow::Cow;

#[derive(Clone)]
pub struct Messenger {
    pub channel: corcovado::channel::Sender<Msg>,
}

impl Messenger {
    pub fn send_clipboard_response(
        &self,
        response: impl Into<std::sync::Arc<rio_backend::clipboard::buffer::ResponseBuffer>>,
    ) {
        let response = response.into();
        if response.is_empty() {
            return;
        }
        let _ = self.channel.send(Msg::ClipboardInput(response));
    }

    pub fn send_clipboard_status(
        &self,
        request: &rio_backend::clipboard::kitty::Request,
        status: rio_backend::clipboard::kitty::Status,
    ) {
        if let Ok(response) = request.status_buffered(status) {
            self.send_clipboard_response(response);
        }
    }

    pub fn send_clipboard_result(
        &self,
        request: &rio_backend::clipboard::kitty::Request,
        response: std::io::Result<rio_backend::clipboard::buffer::ResponseBuffer>,
    ) {
        use rio_backend::clipboard::kitty::Status;
        match response {
            Ok(response) => self.send_clipboard_response(response),
            Err(error) => self.send_clipboard_status(
                request,
                match error.kind() {
                    std::io::ErrorKind::WouldBlock => Status::Busy,
                    std::io::ErrorKind::FileTooLarge => Status::TooLarge,
                    _ => Status::Io,
                },
            ),
        }
    }
    pub fn new(channel: corcovado::channel::Sender<Msg>) -> Messenger {
        Messenger { channel }
    }

    #[inline]
    pub fn send_write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        let bytes = bytes.into();
        // terminal hangs if we send 0 bytes through.
        if bytes.is_empty() {
            return;
        }

        let _ = self.channel.send(Msg::Input(bytes));
    }

    #[inline]
    pub fn send_resize(&self, new_size: WindowSize) -> Result<&str, String> {
        match self.channel.send(Msg::Resize(new_size)) {
            Ok(..) => Ok("Resized"),
            Err(..) => Err("Error sending message".to_string()),
        }
    }
}
