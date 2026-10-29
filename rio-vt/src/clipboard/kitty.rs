//! OSC 5522 request metadata. Platform clipboard access belongs to the frontend.
use base64::{engine::general_purpose::STANDARD, Engine};

use super::ClipboardType;

mod write;
pub use write::{CommittedWrite, Content, WriteError, WriteTransaction};
mod response;
pub use response::{write_read_response, Response, Status, Terminator};
mod session;
pub use session::{Effect, Request, Session};
mod grants;
pub use grants::{Direction, Grants};
mod admission;
pub use admission::{Admission, Pool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
    Data,
    Alias,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub operation: Operation,
    pub location: ClipboardType,
    pub id: String,
    pub mime: String,
    pub password: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    InvalidValue,
}

impl Metadata {
    pub fn operation(raw: &[u8]) -> Option<Operation> {
        let mut operation = None;
        for record in raw.split(|&b| b == b':') {
            let separator = record.iter().position(|&b| b == b'=')?;
            if &record[..separator] == b"type" {
                operation = match &record[separator + 1..] {
                    b"read" => Some(Operation::Read),
                    b"write" => Some(Operation::Write),
                    b"wdata" => Some(Operation::Data),
                    b"walias" => Some(Operation::Alias),
                    _ => None,
                };
            }
        }
        operation
    }
    /// Unknown operations or malformed records are silently ignored. Invalid
    /// encoded values are distinguished so a caller can abort a pending write.
    /// Bounds match Ghostty's clipboard_command.zig and apply before decoding.
    pub fn parse(raw: &[u8]) -> Result<Option<Self>, MetadataError> {
        let mut operation = &b""[..];
        let mut location = &b""[..];
        let mut id = &b""[..];
        let mut mime = &b""[..];
        let mut password = &b""[..];
        let mut name = &b""[..];
        for record in raw.split(|&byte| byte == b':') {
            let Some(separator) = record.iter().position(|&byte| byte == b'=') else {
                return Ok(None);
            };
            let value = &record[separator + 1..];
            match &record[..separator] {
                b"type" => operation = value,
                b"loc" => location = value,
                b"id" => id = value,
                b"mime" => mime = value,
                b"pw" => password = value,
                b"name" => name = value,
                _ => (),
            }
        }
        let operation = match operation {
            b"read" => Operation::Read,
            b"write" => Operation::Write,
            b"wdata" => Operation::Data,
            b"walias" => Operation::Alias,
            _ => return Ok(None),
        };
        // An overlong password never matches a session grant. Still validate
        // bounded inputs so malformed base64 cannot become a valid request.
        let password = if password.len() > 128_usize.div_ceil(3) * 4 {
            String::new()
        } else {
            let decoded = decode_text(password, 129)?;
            if decoded.len() > 128 {
                String::new()
            } else {
                decoded
            }
        };
        let name = decode_text(name, 256)?;
        Ok(Some(Self {
            operation,
            location: if location == b"primary" {
                ClipboardType::Selection
            } else {
                ClipboardType::Clipboard
            },
            id: id
                .iter()
                .copied()
                .filter(|byte| byte.is_ascii_alphanumeric() || b"-_+.".contains(byte))
                .take(512)
                .map(char::from)
                .collect(),
            mime: decode_text(mime, 256)?,
            password: if name.is_empty() {
                String::new()
            } else {
                password
            },
            name,
        }))
    }
}

fn decode_text(raw: &[u8], limit: usize) -> Result<String, MetadataError> {
    if raw.len() > limit.div_ceil(3) * 4 {
        return Err(MetadataError::InvalidValue);
    }
    let decoded = STANDARD
        .decode(raw)
        .map_err(|_| MetadataError::InvalidValue)?;
    if decoded.len() > limit {
        return Err(MetadataError::InvalidValue);
    }
    String::from_utf8(decoded).map_err(|_| MetadataError::InvalidValue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_and_preserves_transaction_identity() {
        let meta = Metadata::parse(b"type=write:loc=primary:id=a!b_+.=:mime=dGV4dC9wbGFpbg==:name=YXBw:pw=c2VjcmV0").unwrap().unwrap();
        assert_eq!(meta.operation, Operation::Write);
        assert_eq!(meta.location, ClipboardType::Selection);
        assert_eq!(meta.id, "ab_+.");
        assert_eq!(meta.mime, "text/plain");
        assert_eq!(meta.name, "app");
        assert_eq!(meta.password, "secret");
    }

    #[test]
    fn validates_structure_and_uses_last_duplicate() {
        assert_eq!(Metadata::parse(b"type=read:broken"), Ok(None));
        assert_eq!(Metadata::parse(b"type=unknown"), Ok(None));
        let meta =
            Metadata::parse(b"type=write:type=wdata:mime=***:mime=:future=x:loc=other")
                .unwrap()
                .unwrap();
        assert_eq!(meta.operation, Operation::Data);
        assert_eq!(meta.mime, "");
        assert_eq!(meta.location, ClipboardType::Clipboard);
    }

    #[test]
    fn malformed_encoding_and_oversized_values_fail() {
        for raw in [b"type=read:mime=***".as_slice(), b"type=read:name=/w=="] {
            assert_eq!(Metadata::parse(raw), Err(MetadataError::InvalidValue));
        }
        let value = STANDARD.encode(vec![b'a'; 257]);
        assert_eq!(
            Metadata::parse(format!("type=read:mime={value}").as_bytes()),
            Err(MetadataError::InvalidValue)
        );
        let meta =
            Metadata::parse(format!("type=write:id={}", "a".repeat(600)).as_bytes())
                .unwrap()
                .unwrap();
        assert_eq!(meta.id.len(), 512);
    }

    #[test]
    fn password_requires_a_name_and_is_bounded() {
        let meta = Metadata::parse(b"type=read:pw=c2VjcmV0").unwrap().unwrap();
        assert!(meta.password.is_empty());
        let password = STANDARD.encode(vec![b'a'; 129]);
        let meta =
            Metadata::parse(format!("type=read:name=YXBw:pw={password}").as_bytes())
                .unwrap()
                .unwrap();
        assert!(meta.password.is_empty());
    }
}
