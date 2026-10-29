//! OSC 7501 reports and the bounded, per-terminal program status store.
//! Protocol: https://www.superlogical.com/rex/docs/build/program-status
//! Keep task state outside either screen buffer so TUIs and SSH can switch
//! screens without losing it. Prompt completion clears activity, not results.

use crate::event::{ProgressReport, ProgressState};
use crate::time::Instant;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;

pub const MAX_SEQUENCE_BYTES: usize = 4096;
const MAX_RECORDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusRecord {
    /// Empty for the root record.
    pub id: String,
    pub state: ProgramState,
    pub kind: Option<BlockedKind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// Parse a complete body before applying anything to the terminal.
pub(crate) fn parse_report(body: &[u8]) -> Option<ProgramStatusRecord> {
    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut msg = None;

    for pair in body.split(|b| *b == b':') {
        let Some((key, value)) = pair
            .iter()
            .position(|b| *b == b'=')
            .map(|i| (pair[..i].trim_ascii(), pair[i + 1..].trim_ascii()))
        else {
            continue;
        };
        if key.len() > 16 {
            return None;
        }
        // Check every occurrence, including values a later pair replaces.
        match key {
            b"msg" if value.len() > 2732 => return None,
            b"title" if value.len() > 256 => return None,
            b"app" if value.len() > 32 => return None,
            b"id"
                if value.len() > 128
                    || value.split(|b| *b == b'/').count() > 8
                    || !value.split(|b| *b == b'/').all(valid_segment) =>
            {
                // Reject before skipping invalid fields: a bad id must not clear root.
                return None;
            }
            _ => (),
        }
        if key.is_empty()
            || !key.iter().all(u8::is_ascii_lowercase)
            || !value
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(b))
        {
            continue;
        }
        let value_str = std::str::from_utf8(value).ok()?;
        match key {
            b"state" => {
                state = match value {
                    b"idle" => Some(ProgramState::Idle),
                    b"working" => Some(ProgramState::Working),
                    b"done" => Some(ProgramState::Done),
                    b"blocked" => Some(ProgramState::Blocked),
                    b"error" => Some(ProgramState::Error),
                    b"clear" => Some(ProgramState::Clear),
                    _ => None,
                }
            }
            b"id" => id = Some(value_str),
            b"kind" => {
                kind = match value {
                    b"permission" => Some(BlockedKind::Permission),
                    b"question" => Some(BlockedKind::Question),
                    b"auth" => Some(BlockedKind::Auth),
                    _ => None,
                }
            }
            b"progress" => {
                progress = value_str
                    .parse::<u8>()
                    .ok()
                    .filter(|p| *p <= 100 && value.iter().all(u8::is_ascii_digit))
            }
            b"app" => app = valid_segment(value).then(|| value_str.to_owned()),
            b"title" => title = Some(decode_text(value, 192)?),
            b"msg" => msg = Some(decode_text(value, 2048)?),
            _ => (),
        }
    }
    let state = state?;
    Some(ProgramStatusRecord {
        id: id.unwrap_or_default().to_owned(),
        state,
        kind: (state == ProgramState::Blocked).then_some(kind).flatten(),
        progress: matches!(state, ProgramState::Working | ProgramState::Blocked)
            .then_some(progress)
            .flatten(),
        app,
        title,
        msg,
    })
}

fn valid_segment(segment: &[u8]) -> bool {
    !segment.is_empty()
        && segment.len() <= 32
        && segment
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(b))
}

fn decode_text(encoded: &[u8], limit: usize) -> Option<String> {
    let engine = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = engine.decode(encoded).ok()?;
    if bytes.len() > limit {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}

#[derive(Debug, Default)]
pub struct ProgramStatusStore {
    // Oldest update first. A small bounded vector also makes subtree clears
    // and deterministic display selection straightforward.
    records: Vec<ProgramStatusRecord>,
    received_report: bool,
    legacy_progress: Option<(ProgressReport, Instant)>,
    // Clearing legacy progress must not reveal a previous command's native result.
    legacy_owns_display: bool,
    revision: u64,
}

impl ProgramStatusStore {
    pub fn records(&self) -> &[ProgramStatusRecord] {
        &self.records
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn received_report(&self) -> bool {
        self.received_report
    }

    pub fn legacy_progress(&self) -> Option<(ProgressReport, Instant)> {
        self.legacy_progress
    }

    /// One status for the tab pill, regardless of the reporting protocol.
    pub fn display_status(&self) -> Option<ProgramStatusRecord> {
        if !self.legacy_owns_display {
            let record = self.current()?;
            let mut record = record.clone();
            record.app = self.app_for(&record.id).map(str::to_owned);
            return Some(record);
        }
        let (report, _) = self.legacy_progress?;
        let state = match report.state {
            ProgressState::Set | ProgressState::Indeterminate => ProgramState::Working,
            ProgressState::Error => ProgramState::Error,
            ProgressState::Pause => ProgramState::Blocked,
            ProgressState::Remove => return None,
        };
        Some(ProgramStatusRecord {
            id: String::new(),
            state,
            kind: None,
            progress: report.progress,
            app: None,
            title: None,
            msg: (report.state == ProgressState::Pause).then(|| "Paused".into()),
        })
    }

    pub(crate) fn apply(&mut self, report: ProgramStatusRecord) {
        self.received_report = true;
        self.legacy_progress = None;
        self.legacy_owns_display = false;
        if report.state == ProgramState::Clear {
            self.records.retain(|r| !is_descendant(&r.id, &report.id));
        } else {
            self.records.retain(|r| r.id != report.id);
            if self.records.len() == MAX_RECORDS {
                self.records.remove(0);
            }
            self.records.push(report);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// OSC 9;4 remains a fallback until OSC 7501 reports in this command.
    pub(crate) fn set_legacy_progress(&mut self, report: ProgressReport) -> bool {
        if self.received_report {
            return false;
        }
        self.legacy_owns_display = true;
        self.legacy_progress =
            (report.state != ProgressState::Remove).then(|| (report, Instant::now()));
        self.revision = self.revision.wrapping_add(1);
        true
    }

    pub(crate) fn end_activity(&mut self) -> bool {
        let old_len = self.records.len();
        self.records.retain(|r| {
            !matches!(r.state, ProgramState::Working | ProgramState::Blocked)
        });
        let had_legacy_progress = self.legacy_progress.take().is_some();
        let received_report = std::mem::take(&mut self.received_report);
        let changed =
            old_len != self.records.len() || had_legacy_progress || received_report;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    pub(crate) fn reset(&mut self) {
        self.records.clear();
        self.received_report = false;
        self.legacy_progress = None;
        self.legacy_owns_display = false;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Prefer tasks that need input, then failures, active work, completion,
    /// and idle. Within a state, show the most recently updated record.
    pub fn current(&self) -> Option<&ProgramStatusRecord> {
        self.records.iter().max_by_key(|r| match r.state {
            ProgramState::Blocked => 5,
            ProgramState::Error => 4,
            ProgramState::Working => 3,
            ProgramState::Done => 2,
            ProgramState::Idle => 1,
            ProgramState::Clear => 0,
        })
    }

    pub fn app_for(&self, id: &str) -> Option<&str> {
        let mut ancestor = id;
        loop {
            if let Some(app) = self
                .records
                .iter()
                .find(|r| r.id == ancestor)
                .and_then(|r| r.app.as_deref())
            {
                return Some(app);
            }
            if ancestor.is_empty() {
                return None;
            }
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
}

fn is_descendant(id: &str, parent: &str) -> bool {
    parent.is_empty()
        || id == parent
        || id
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(body: &str) -> ProgramStatusRecord {
        parse_report(body.as_bytes()).unwrap()
    }

    #[test]
    fn parses_optional_padding_duplicates_and_unknown_fields() {
        let r =
            report(" state =working:unknown=value:bad:msg=aGk:progress=4:progress=80");
        assert_eq!(r.msg.as_deref(), Some("hi"));
        assert_eq!(r.progress, Some(80));
        assert_eq!(
            report("state=blocked:kind=auth:app=cargo").kind,
            Some(BlockedKind::Auth)
        );
        assert_eq!(report("state=done:kind=auth:progress=10").progress, None);
        assert_eq!(report("state=working:progress=101").progress, None);
        assert!(parse_report(b"state=working:state=future").is_none());
        assert_eq!(report("state=working:progress=+1").progress, None);
    }

    #[test]
    fn rejects_bad_text_ids_and_limits_before_applying() {
        for body in [
            "state=done:msg=AA==",
            "state=done:msg=wok=",
            "state=done:msg=/w==",
            "state=done:msg=A",
            "state=done:id=",
            "state=done:id=a//b",
            "state=done:id=a,b",
            "state=clear:id=build!",
            "state=clear:id=build test",
            "state=clear:id=build\u{00e9}",
        ] {
            assert!(parse_report(body.as_bytes()).is_none(), "{body}");
        }
        for body in [
            format!("state=done:msg={}:msg=aGk=", "A".repeat(2733)),
            format!("state=done:app={}", "a".repeat(33)),
            format!("state=done:title={}", "A".repeat(257)),
            format!("state=done:{}=x", "k".repeat(17)),
            "state=done:id=a/b/c/d/e/f/g/h/i".to_owned(),
            format!("state=done:id={}", "a".repeat(33)),
        ] {
            assert!(parse_report(body.as_bytes()).is_none(), "{body}");
        }
        assert!(parse_report(b"state=done:msg=A:msg=aGk=").is_none());
        assert_eq!(report("state=done:msg=aGk!:app=cargo").msg, None);
    }

    #[test]
    fn replaces_records_inherits_app_and_clears_only_subtree() {
        let mut store = ProgramStatusStore::default();
        store.apply(report("state=working:app=cargo"));
        store.apply(report("state=working:id=build:app=compiler"));
        store.apply(report("state=blocked:id=build/test:msg=aGk="));
        store.apply(report("state=done:id=builder"));
        assert_eq!(store.current().unwrap().id, "build/test");
        assert_eq!(store.app_for("build/test"), Some("compiler"));
        store.apply(report("state=working:id=build"));
        assert_eq!(store.app_for("build/test"), Some("cargo"));
        store.apply(report("state=clear:id=build"));
        assert_eq!(store.records().len(), 2);
        assert!(store.records().iter().any(|r| r.id == "builder"));
        store.apply(report("state=clear"));
        assert!(store.records().is_empty());
    }

    #[test]
    fn evicts_least_recently_updated_record() {
        let mut store = ProgramStatusStore::default();
        for i in 0..MAX_RECORDS {
            store.apply(report(&format!("state=working:id={i}")));
        }
        store.apply(report("state=done:id=0"));
        store.apply(report("state=working:id=new"));
        assert_eq!(store.records().len(), MAX_RECORDS);
        assert!(store.records().iter().any(|r| r.id == "0"));
        assert!(!store.records().iter().any(|r| r.id == "1"));
    }

    #[test]
    fn text_and_id_limits_accept_boundaries_and_reject_decoded_overflow() {
        use base64::engine::general_purpose::STANDARD;
        let msg = "a".repeat(2048);
        let title = "b".repeat(192);
        let id = format!(
            "{}/{}/{}/{}",
            "c".repeat(32),
            "d".repeat(32),
            "e".repeat(32),
            "f".repeat(29)
        );
        assert_eq!(id.len(), 128);
        let parsed = report(&format!(
            "state=working:id={id}:msg={}:title={}",
            STANDARD.encode(msg.as_bytes()),
            STANDARD.encode(title.as_bytes())
        ));
        assert_eq!(parsed.msg.as_deref(), Some(msg.as_str()));
        assert_eq!(parsed.title.as_deref(), Some(title.as_str()));
        // Both encode to 2732 bytes, so the decoded limit matters as well.
        let oversized = STANDARD.encode("a".repeat(2049));
        assert_eq!(oversized.len(), 2732);
        assert!(parse_report(format!("state=done:msg={oversized}").as_bytes()).is_none());
        let mut store = ProgramStatusStore::default();
        store.apply(report("state=working:id=first"));
        store.apply(report("state=working:id=second"));
        assert_eq!(store.current().unwrap().id, "second");
        store.apply(report("state=working:id=first"));
        assert_eq!(store.current().unwrap().id, "first");
    }

    #[test]
    fn lifecycle_preserves_results_and_reset_restores_legacy_progress() {
        let mut store = ProgramStatusStore::default();
        let legacy = ProgressReport {
            state: ProgressState::Set,
            progress: Some(50),
        };
        assert!(store.set_legacy_progress(legacy));
        for state in ["working", "blocked", "idle", "done", "error"] {
            store.apply(report(&format!("state={state}:id={state}")));
        }
        assert!(!store.set_legacy_progress(legacy));
        assert!(store.end_activity());
        assert_eq!(store.records().len(), 3);
        assert!(!store.end_activity());
        store.reset();
        assert!(store.records().is_empty());
        assert!(store.set_legacy_progress(legacy));
    }
}
