use super::{Content, Metadata, Operation};
use crate::clipboard::ClipboardType;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    Bell,
    String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Data,
    Done,
    Busy,
    TooLarge,
    Invalid,
    Io,
    Unsupported,
    Permission,
    Ok,
}

pub struct Response<'a> {
    pub metadata: &'a Metadata,
    pub status: Status,
    pub primary: bool,
    pub mime: Option<&'a str>,
    pub password: Option<&'a str>,
    pub payload: &'a [u8],
    pub terminator: Terminator,
}

impl Response<'_> {
    pub fn encode(&self, writer: &mut impl Write) -> io::Result<()> {
        let operation = match self.metadata.operation {
            Operation::Read => "read",
            Operation::Write => "write",
            Operation::Data => "wdata",
            Operation::Alias => "walias",
        };
        let status = match self.status {
            Status::Data => "DATA",
            Status::Done => "DONE",
            Status::Busy => "EBUSY",
            Status::TooLarge => "EFBIG",
            Status::Invalid => "EINVAL",
            Status::Io => "EIO",
            Status::Unsupported => "ENOSYS",
            Status::Permission => "EPERM",
            Status::Ok => "OK",
        };
        write!(writer, "\x1b]5522;type={operation}:status={status}")?;
        if self.primary {
            writer.write_all(b":loc=primary")?;
        }
        if !self.metadata.id.is_empty() {
            writer.write_all(b":id=")?;
            for byte in self
                .metadata
                .id
                .bytes()
                .filter(|byte| byte.is_ascii_alphanumeric() || b"-_+.".contains(byte))
                .take(512)
            {
                writer.write_all(&[byte])?;
            }
        }
        if let Some(mime) = self.mime {
            write!(writer, ":mime={}", STANDARD.encode(mime))?;
        }
        if let Some(password) = self.password {
            write!(writer, ":pw={}", STANDARD.encode(password))?;
        }
        if !self.payload.is_empty() {
            write!(writer, ";{}", STANDARD.encode(self.payload))?;
        }
        writer.write_all(match self.terminator {
            Terminator::Bell => b"\x07",
            Terminator::String => b"\x1b\\",
        })
    }
}

/// Stream successful reads in protocol-sized packets. `contents` must already
/// be selected in request order by the permission-aware platform provider.
pub fn write_read_response(
    writer: &mut impl Write,
    metadata: &Metadata,
    terminator: Terminator,
    password: Option<&str>,
    available: Option<&[String]>,
    contents: &[Content],
) -> io::Result<()> {
    let mut packet = Response {
        metadata,
        status: Status::Ok,
        primary: metadata.location == ClipboardType::Selection,
        mime: None,
        password,
        payload: &[],
        terminator,
    };
    packet.encode(writer)?;
    packet.primary = false;
    packet.status = Status::Data;
    if let Some(available) = available {
        let mut listing = Vec::new();
        for mime in available.iter().take(16) {
            if listing
                .len()
                .saturating_add(mime.len())
                .saturating_add(if listing.is_empty() { 1 } else { 2 })
                > 4096
            {
                break;
            }
            if !listing.is_empty() {
                listing.push(b' ');
            }
            listing.extend_from_slice(mime.as_bytes());
        }
        if !listing.is_empty() {
            listing.push(b'\n');
        }
        Response {
            mime: Some("."),
            payload: &listing,
            ..packet
        }
        .encode(writer)?;
    }
    for content in contents.iter().take(4) {
        for chunk in content.data.chunks(4096) {
            Response {
                mime: Some(&content.mime),
                payload: chunk,
                ..packet
            }
            .encode(writer)?;
        }
    }
    packet.status = Status::Done;
    packet.encode(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn processor_dispatches_split_osc_and_both_terminators() {
        use crate::performer::handler::{Handler, Processor};
        #[derive(Default)]
        struct Capture(Vec<(Vec<u8>, Vec<u8>, String)>);
        impl Handler for Capture {
            fn kitty_clipboard(
                &mut self,
                metadata: &[u8],
                payload: &[u8],
                terminator: &str,
            ) {
                self.0
                    .push((metadata.to_vec(), payload.to_vec(), terminator.to_owned()));
            }
        }
        let mut processor = Processor::default();
        let mut capture = Capture::default();
        for byte in b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;YQ==\x1b\\\x1b]5522;type=read\x07" {
            processor.advance(&mut capture, &[*byte]);
        }
        assert_eq!(
            capture.0,
            vec![
                (
                    b"type=wdata:mime=dGV4dC9wbGFpbg==".to_vec(),
                    b"YQ==".to_vec(),
                    "\x1b\\".into()
                ),
                (b"type=read".to_vec(), Vec::new(), "\x07".into()),
            ]
        );
    }
    #[test]
    fn exact_read_framing_and_primary_scope() {
        let metadata = Metadata::parse(b"type=read:loc=primary:id=42")
            .unwrap()
            .unwrap();
        let contents = [Content {
            mime: "text/plain".into(),
            data: Arc::new(b"hello".to_vec()),
        }];
        let mut output = Vec::new();
        write_read_response(
            &mut output,
            &metadata,
            Terminator::String,
            None,
            Some(&["text/plain".into()]),
            &contents,
        )
        .unwrap();
        assert_eq!(
            output,
            concat!("\x1b]5522;type=read:status=OK:loc=primary:id=42\x1b\\",
            "\x1b]5522;type=read:status=DATA:id=42:mime=Lg==;dGV4dC9wbGFpbgo=\x1b\\",
            "\x1b]5522;type=read:status=DATA:id=42:mime=dGV4dC9wbGFpbg==;aGVsbG8=\x1b\\",
            "\x1b]5522;type=read:status=DONE:id=42\x1b\\")
            .as_bytes()
        );
    }

    #[test]
    fn chunk_boundaries_and_bell_terminator() {
        let metadata = Metadata::parse(b"type=read").unwrap().unwrap();
        let contents = [Content {
            mime: "x/a".into(),
            data: Arc::new(vec![b'a'; 4097]),
        }];
        let mut output = Vec::new();
        write_read_response(
            &mut output,
            &metadata,
            Terminator::Bell,
            Some("secret"),
            None,
            &contents,
        )
        .unwrap();
        assert_eq!(output.iter().filter(|&&b| b == 7).count(), 4);
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.matches(":pw=c2VjcmV0").count(), 4);
        assert!(text.contains(";YQ==\x07"));
    }
}
