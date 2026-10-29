use super::{Metadata, Operation};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::sync::Arc;

/// A MIME representation. Aliases share byte ownership without copying.
#[derive(Debug)]
pub struct Content {
    pub mime: String,
    pub data: Arc<Vec<u8>>,
}

#[derive(Debug)]
pub struct CommittedWrite {
    pub metadata: Metadata,
    pub contents: Vec<Content>,
    pub(super) _admission: super::Admission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    Busy,
    Invalid,
    TooLarge,
}

/// One unpublished write. Errors poison the transaction and release staged
/// buffers. Only consuming `commit` exposes data to a platform provider.
pub struct WriteTransaction {
    metadata: Metadata,
    entries: Vec<(String, Vec<u8>)>,
    aliases: Vec<(String, String)>,
    current: Option<usize>,
    carry: [u8; 4],
    carry_len: usize,
    received: usize,
    limit: usize,
    failed: Option<WriteError>,
    admission: Option<super::Admission>,
}

impl WriteTransaction {
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn new(metadata: Metadata, limit: usize) -> Result<Self, WriteError> {
        Self::new_in(metadata, limit, &super::Pool::default())
    }
    pub(super) fn new_in(
        metadata: Metadata,
        limit: usize,
        pool: &super::Pool,
    ) -> Result<Self, WriteError> {
        if metadata.operation != Operation::Write {
            return Err(WriteError::Invalid);
        }
        Ok(Self {
            admission: Some(pool.admit()?),
            metadata,
            entries: Vec::new(),
            aliases: Vec::new(),
            current: None,
            carry: [0; 4],
            carry_len: 0,
            received: 0,
            limit,
            failed: None,
        })
    }

    fn fail<T>(&mut self, error: WriteError) -> Result<T, WriteError> {
        self.failed = Some(error);
        self.entries = Vec::new();
        self.aliases = Vec::new();
        self.admission = None;
        self.current = None;
        self.carry_len = 0;
        Err(error)
    }

    pub fn data(&mut self, mime: &str, payload: &[u8]) -> Result<(), WriteError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if mime.is_empty() || mime.len() > 256 {
            return self.fail(WriteError::Invalid);
        }
        let same = self.current.is_some_and(|i| self.entries[i].0 == mime);
        if !same {
            if self.carry_len != 0 {
                return self.fail(WriteError::Invalid);
            }
            if let Some(current) = self.current {
                let capacity = self.entries[current].1.capacity();
                self.entries[current].1.shrink_to_fit();
                self.admission
                    .as_mut()
                    .unwrap()
                    .release(capacity - self.entries[current].1.capacity());
            }
            let index = if let Some(i) = self.entries.iter().position(|e| e.0 == mime) {
                let previous = std::mem::take(&mut self.entries[i].1);
                let capacity = previous.capacity();
                drop(previous);
                self.admission.as_mut().unwrap().release(capacity);
                i
            } else {
                if self.entries.len() == 64 {
                    self.current = None;
                    return Ok(());
                }
                self.entries.push((mime.to_owned(), Vec::new()));
                self.entries.len() - 1
            };
            self.current = Some(index);
        }
        let index = self.current.unwrap();
        for (position, &byte) in payload.iter().enumerate() {
            self.carry[self.carry_len] = byte;
            self.carry_len += 1;
            if self.carry_len != 4 {
                continue;
            }
            let mut decoded = [0; 3];
            let len = match STANDARD.decode_slice(self.carry, &mut decoded) {
                Ok(len) => len,
                Err(_) => return self.fail(WriteError::Invalid),
            };
            // Padding ends this packet, but independently padded packets are
            // permitted for compatibility with Kitty/Ghostty streaming writes.
            if self.carry[3] == b'=' && position + 1 != payload.len() {
                return self.fail(WriteError::Invalid);
            }
            if len > self.limit.saturating_sub(self.received) {
                return self.fail(WriteError::TooLarge);
            }
            self.received += len;
            let data = &mut self.entries[index].1;
            let needed = data.len() + len;
            if needed > data.capacity() {
                let admission = self.admission.as_mut().unwrap();
                let capacity = needed
                    .max(data.capacity().saturating_mul(2))
                    .max(4096)
                    .min(self.limit)
                    .min(data.capacity().saturating_add(admission.available()));
                if capacity < needed {
                    return self.fail(WriteError::Busy);
                }
                let growth = capacity - data.capacity();
                if admission.reserve(growth).is_err() {
                    return self.fail(WriteError::Busy);
                }
                if data.try_reserve_exact(capacity - data.len()).is_err() {
                    return self.fail(WriteError::Busy);
                }
            }
            self.entries[index].1.extend_from_slice(&decoded[..len]);
            self.carry_len = 0;
        }
        Ok(())
    }

    pub fn alias(&mut self, target: &str, payload: &[u8]) -> Result<(), WriteError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if target.is_empty() || target.len() > 256 {
            return self.fail(WriteError::Invalid);
        }
        // Bound temporary decoded alias text as well as retained metadata.
        if payload.len() > 64 * 1024 {
            return self.fail(WriteError::TooLarge);
        }
        let decoded = match STANDARD.decode(payload) {
            Ok(data) => data,
            Err(_) => return self.fail(WriteError::Invalid),
        };
        let text = match std::str::from_utf8(&decoded) {
            Ok(text) => text,
            Err(_) => return self.fail(WriteError::Invalid),
        };
        for name in text.split_ascii_whitespace() {
            if name.len() > 256 {
                continue;
            }
            if let Some(entry) = self.aliases.iter_mut().find(|a| a.0 == name) {
                entry.1 = target.to_owned();
            } else if self.aliases.len() < 64 {
                self.aliases.push((name.to_owned(), target.to_owned()));
            }
        }
        Ok(())
    }

    pub fn commit(mut self) -> Result<CommittedWrite, WriteError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        if self.carry_len != 0 {
            return Err(WriteError::Invalid);
        }
        for (_, data) in &mut self.entries {
            let capacity = data.capacity();
            data.shrink_to_fit();
            self.admission
                .as_mut()
                .unwrap()
                .release(capacity - data.capacity());
        }
        let mut contents: Vec<_> = self
            .entries
            .into_iter()
            .map(|(mime, data)| Content {
                mime,
                data: Arc::new(data),
            })
            .collect();
        for (alias, target) in self.aliases {
            let Some(data) = contents
                .iter()
                .find(|c| c.mime == target)
                .map(|c| c.data.clone())
            else {
                continue;
            };
            if let Some(entry) = contents.iter_mut().find(|c| c.mime == alias) {
                entry.data = data;
            } else {
                contents.push(Content { mime: alias, data });
            }
        }
        Ok(CommittedWrite {
            _admission: self.admission.take().unwrap(),
            metadata: self.metadata,
            contents,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_aliases_retain_one_buffer_reservation_until_release() {
        let pool = super::super::Pool::new(2, 8);
        let metadata = Metadata::parse(b"type=write").unwrap().unwrap();
        let mut tx = WriteTransaction::new_in(metadata, 8, &pool).unwrap();
        tx.data("x/a", b"YWJjZA==").unwrap();
        tx.alias("x/a", b"eC9hbGlhcw==").unwrap();
        let committed = tx.commit().unwrap();
        assert!(Arc::ptr_eq(
            &committed.contents[0].data,
            &committed.contents[1].data
        ));
        let mut probe = pool.admit().unwrap();
        assert_eq!(probe.available(), 4);
        probe.reserve(4).unwrap();
        assert!(probe.reserve(1).is_err());
        drop(probe);
        drop(committed);
        let mut recovered = pool.admit().unwrap();
        recovered.reserve(8).unwrap();
    }

    #[test]
    fn byte_pressure_aborts_and_releases_slot_and_staged_capacity() {
        let pool = super::super::Pool::new(1, 4);
        let mut tx = WriteTransaction::new_in(
            Metadata::parse(b"type=write").unwrap().unwrap(),
            10,
            &pool,
        )
        .unwrap();
        assert_eq!(tx.data("x/a", b"SGVsbG8="), Err(WriteError::Busy));
        assert!(tx.entries.is_empty());
        let mut probe = pool.admit().unwrap();
        probe.reserve(4).unwrap();
        assert!(matches!(tx.commit(), Err(WriteError::Busy)));
    }

    #[test]
    fn mime_switch_and_replacement_reclaim_spare_capacity() {
        let pool = super::super::Pool::new(2, 12);
        let mut tx = WriteTransaction::new_in(
            Metadata::parse(b"type=write").unwrap().unwrap(),
            20,
            &pool,
        )
        .unwrap();
        tx.data("x/a", b"YWJjZA==").unwrap();
        tx.data("x/b", b"ZWZnaA==").unwrap();
        tx.data("x/a", b"aWo=").unwrap();
        let committed = tx.commit().unwrap();
        assert_eq!(committed.contents[0].data.as_slice(), b"ij");
        let mut probe = pool.admit().unwrap();
        assert_eq!(probe.available(), 6);
        probe.reserve(6).unwrap();
    }
    fn transaction(limit: usize) -> WriteTransaction {
        WriteTransaction::new(
            Metadata::parse(b"type=write:id=42").unwrap().unwrap(),
            limit,
        )
        .unwrap()
    }

    #[test]
    fn all_packet_boundaries_and_padded_packets() {
        for split in 0..=8 {
            let mut tx = transaction(10);
            tx.data("text/plain", &b"SGVsbG8="[..split]).unwrap();
            tx.data("text/plain", &b"SGVsbG8="[split..]).unwrap();
            tx.data("text/plain", b"V29ybGQ=").unwrap();
            let result = tx.commit().unwrap();
            assert_eq!(result.metadata.id, "42");
            assert_eq!(result.contents[0].data.as_slice(), b"HelloWorld");
        }
    }

    #[test]
    fn aliases_chain_and_share_payloads_without_copying() {
        let mut tx = transaction(1);
        tx.data("image/png", b"YQ==").unwrap();
        tx.alias("image/png", b"eC9h").unwrap(); // x/a
        tx.alias("x/a", b"eC9i").unwrap(); // x/b
        let result = tx.commit().unwrap();
        assert_eq!(result.contents.len(), 3);
        assert!(Arc::ptr_eq(
            &result.contents[0].data,
            &result.contents[2].data
        ));
    }

    #[test]
    fn failures_abort_and_release_all_data() {
        for bad in [b"***=".as_slice(), b"YQ==Yg=="] {
            let mut tx = transaction(10);
            tx.data("a", b"YQ==").unwrap();
            assert_eq!(tx.data("b", bad), Err(WriteError::Invalid));
            assert!(tx.entries.is_empty());
            assert!(matches!(tx.commit(), Err(WriteError::Invalid)));
        }
        let mut tx = transaction(1);
        tx.data("a", b"YQ==").unwrap();
        assert_eq!(tx.data("b", b"Yg=="), Err(WriteError::TooLarge));
        assert!(matches!(tx.commit(), Err(WriteError::TooLarge)));
    }

    #[test]
    fn partial_groups_fail_on_switch_or_commit() {
        let mut tx = transaction(10);
        tx.data("a", b"YQ").unwrap();
        assert_eq!(tx.data("b", b"Yg=="), Err(WriteError::Invalid));
        let mut tx = transaction(10);
        tx.data("a", b"YQ").unwrap();
        assert!(matches!(tx.commit(), Err(WriteError::Invalid)));
    }

    #[test]
    fn repeated_mime_replaces_but_still_counts_received_bytes() {
        let mut tx = transaction(3);
        tx.data("a", b"YQ==").unwrap();
        tx.data("b", b"Yg==").unwrap();
        tx.data("a", b"Yw==").unwrap();
        let result = tx.commit().unwrap();
        assert_eq!(result.contents[0].data.as_slice(), b"c");
        assert_eq!(result.contents[1].data.as_slice(), b"b");
    }

    #[test]
    fn mime_and_alias_counts_are_bounded() {
        let mut tx = transaction(100);
        for i in 0..100 {
            tx.data(&format!("x/{i}"), b"YQ==").unwrap();
            tx.alias("x/0", STANDARD.encode(format!("alias/{i}")).as_bytes())
                .unwrap();
        }
        assert_eq!(tx.entries.len(), 64);
        assert_eq!(tx.aliases.len(), 64);
        assert_eq!(tx.commit().unwrap().contents.len(), 128);
    }

    #[test]
    fn aliases_replace_in_order_and_missing_targets_are_ignored() {
        let mut tx = transaction(2);
        tx.data("a", b"YQ==").unwrap();
        tx.data("b", b"Yg==").unwrap();
        tx.alias("missing", b"eA==").unwrap(); // x
        tx.alias("a", b"Yg==").unwrap(); // b
        tx.alias("b", b"eQ==").unwrap(); // y
        let result = tx.commit().unwrap();
        assert_eq!(result.contents.len(), 3);
        assert_eq!(result.contents[1].data.as_slice(), b"a");
        assert!(Arc::ptr_eq(
            &result.contents[1].data,
            &result.contents[2].data
        ));
    }
}
