use std::collections::HashMap;
use std::io::{Error, ErrorKind, Result};
use std::sync::mpsc::Sender;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{
    DataOfferError, DataOfferHandler, DragOffer,
};
use sctk::data_device_manager::data_source::{CopyPasteSource, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::primary_selection::PrimarySelectionManagerState;
use sctk::primary_selection::device::{
    PrimarySelectionDevice, PrimarySelectionDeviceHandler,
};
use sctk::primary_selection::selection::{
    PrimarySelectionSource, PrimarySelectionSourceHandler,
};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{
    delegate_data_device, delegate_pointer, delegate_primary_selection,
    delegate_registry, delegate_seat, registry_handlers,
};

use sctk::reexports::calloop::LoopHandle;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
use sctk::reexports::client::protocol::wl_pointer::WlPointer;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_backend::client::ObjectId;

use crate::mime::ALLOWED_MIME_TYPES;

fn validate_contents(contents: &crate::MimeContents, limit: usize) -> Result<()> {
    let mut names = std::collections::HashSet::new();
    let mut buffers = std::collections::HashSet::new();
    let mut total = 0usize;
    if contents.len() > 128 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "too many MIME representations",
        ));
    }
    for (mime, data) in contents {
        if mime.is_empty()
            || mime.len() > 256
            || mime.bytes().any(|b| b == 0 || b.is_ascii_whitespace())
            || !names.insert(mime)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid MIME representations",
            ));
        }
        if buffers.insert(std::sync::Arc::as_ptr(data)) {
            if data.capacity() > limit.saturating_sub(total) {
                return Err(Error::new(
                    ErrorKind::FileTooLarge,
                    "clipboard publication byte limit exceeded",
                ));
            }
            total += data.capacity();
        }
    }
    Ok(())
}

fn validate_retained_contents<'a>(
    retained: impl Iterator<Item = &'a crate::MimeContents>,
    incoming: &'a crate::MimeContents,
    source_limit: usize,
    byte_limit: usize,
) -> Result<()> {
    let mut buffers = std::collections::HashSet::new();
    let mut total = 0usize;
    let mut sources = 0usize;
    for contents in retained.chain(std::iter::once(incoming)) {
        sources += 1;
        if sources > source_limit {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "too many retained clipboard sources",
            ));
        }
        for (_, data) in contents {
            if buffers.insert(std::sync::Arc::as_ptr(data)) {
                if data.capacity() > byte_limit.saturating_sub(total) {
                    return Err(Error::new(
                        ErrorKind::WouldBlock,
                        "retained clipboard source byte limit exceeded",
                    ));
                }
                total += data.capacity();
            }
        }
    }
    Ok(())
}

pub struct State {
    transfer_budget: crate::transfer::Budget,
    pub primary_selection_manager_state: Option<PrimarySelectionManagerState>,
    pub data_device_manager_state: Option<DataDeviceManagerState>,
    pub exit: bool,

    registry_state: RegistryState,
    seat_state: SeatState,

    seats: HashMap<ObjectId, ClipboardSeatState>,
    /// The latest seat which got an event.
    latest_seat: Option<ObjectId>,

    loop_handle: LoopHandle<'static, Self>,
    queue_handle: QueueHandle<Self>,

    primary_sources: Vec<PrimarySelectionSource>,

    data_sources: Vec<CopyPasteSource>,
    typed_contents: HashMap<ObjectId, std::sync::Arc<crate::MimeContents>>,
}

impl State {
    pub fn acknowledge_store(&self, connection: &Connection, reply: Sender<Result<()>>) {
        connection.display().sync(
            &self.queue_handle,
            StoreAck(std::sync::Mutex::new(Some(reply))),
        );
    }
    fn active_seat(&self) -> Result<&ClipboardSeatState> {
        let seat = self
            .latest_seat
            .as_ref()
            .and_then(|id| self.seats.get(id))
            .ok_or_else(|| Error::other("no active clipboard seat"))?;
        if !seat.has_focus {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "client does not have focus",
            ));
        }
        Ok(seat)
    }

    pub fn offer(&self, target: SelectionTarget) -> Result<crate::Offer> {
        let seat = self.active_seat()?;
        let mut id = crate::OfferId {
            seat: self.latest_seat.as_ref().unwrap().clone(),
            source: match target {
                SelectionTarget::Clipboard => crate::OfferSource::Clipboard(None),
                SelectionTarget::Primary => crate::OfferSource::Primary(None),
            },
        };
        match target {
            SelectionTarget::Clipboard => {
                let device = seat.data_device.as_ref().ok_or_else(|| {
                    Error::new(ErrorKind::Unsupported, "clipboard unavailable")
                })?;
                let mimes =
                    device
                        .data()
                        .selection_offer()
                        .map_or_else(Vec::new, |offer| {
                            id.source =
                                crate::OfferSource::Clipboard(Some(offer.inner().id()));
                            offer.with_mime_types(|types| types.to_vec())
                        });
                Ok(crate::Offer { id, mimes })
            }
            SelectionTarget::Primary => {
                let device = seat.primary_device.as_ref().ok_or_else(|| {
                    Error::new(ErrorKind::Unsupported, "primary selection unavailable")
                })?;
                let mimes =
                    device
                        .data()
                        .selection_offer()
                        .map_or_else(Vec::new, |offer| {
                            id.source = crate::OfferSource::Primary(Some(offer.clone()));
                            offer.with_mime_types(|types| types.to_vec())
                        });
                Ok(crate::Offer { id, mimes })
            }
        }
    }

    pub fn store_mimes(
        &mut self,
        target: SelectionTarget,
        contents: crate::MimeContents,
    ) -> Result<()> {
        validate_contents(&contents, crate::transfer::MAX_BYTES)?;
        // Cancellation events can lag behind publication. Leave room for replacing
        // a full clipboard and primary selection, but bound stalled source retention.
        validate_retained_contents(
            self.typed_contents
                .values()
                .map(|contents| contents.as_ref()),
            &contents,
            16,
            3 * crate::transfer::MAX_BYTES,
        )?;
        let seat = self.active_seat()?;
        let mimes: Vec<_> = contents.iter().map(|(mime, _)| mime.clone()).collect();
        match target {
            SelectionTarget::Clipboard => {
                let manager =
                    self.data_device_manager_state.as_ref().ok_or_else(|| {
                        Error::new(ErrorKind::Unsupported, "clipboard unavailable")
                    })?;
                let device = seat.data_device.as_ref().ok_or_else(|| {
                    Error::new(ErrorKind::Unsupported, "clipboard device unavailable")
                })?;
                let source =
                    manager.create_copy_paste_source(&self.queue_handle, mimes.iter());
                source.set_selection(device, seat.latest_serial);
                self.typed_contents
                    .insert(source.inner().id(), std::sync::Arc::new(contents));
                self.data_sources.push(source);
            }
            SelectionTarget::Primary => {
                let manager =
                    self.primary_selection_manager_state
                        .as_ref()
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::Unsupported,
                                "primary selection unavailable",
                            )
                        })?;
                let device = seat.primary_device.as_ref().ok_or_else(|| {
                    Error::new(ErrorKind::Unsupported, "primary device unavailable")
                })?;
                let source =
                    manager.create_selection_source(&self.queue_handle, mimes.iter());
                source.set_selection(device, seat.latest_serial);
                self.typed_contents
                    .insert(source.inner().id(), std::sync::Arc::new(contents));
                self.primary_sources.push(source);
            }
        }
        Ok(())
    }

    pub fn load_mime(
        &mut self,
        target: SelectionTarget,
        offer: crate::OfferId,
        mime: String,
        limit: usize,
        reply: Sender<Result<Vec<u8>>>,
    ) -> Result<()> {
        if offer != self.offer(target)?.id {
            return Err(Error::new(
                ErrorKind::Interrupted,
                "clipboard offer changed",
            ));
        }
        let seat = self.active_seat()?;
        let pipe = match target {
            SelectionTarget::Clipboard => seat
                .data_device
                .as_ref()
                .and_then(|device| device.data().selection_offer())
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "no clipboard offer"))?
                .receive(mime)
                .map_err(|error| match error {
                    DataOfferError::Io(error) => error,
                    _ => Error::other("offer not ready"),
                })?,
            SelectionTarget::Primary => seat
                .primary_device
                .as_ref()
                .and_then(|device| device.data().selection_offer())
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "no primary offer"))?
                .receive(mime)?,
        };
        crate::transfer::read(
            &self.loop_handle,
            pipe,
            limit,
            &self.transfer_budget,
            std::time::Duration::from_secs(5),
            reply,
        )
    }

    fn send_typed(&mut self, source: &ObjectId, mime: &str, pipe: WritePipe) {
        let Some(contents) = self
            .typed_contents
            .get(source)
            .and_then(|contents| contents.iter().find(|(name, _)| name == mime))
            .map(|(_, data)| data.clone())
        else {
            return;
        };
        let _ = crate::transfer::write(
            &self.loop_handle,
            pipe,
            contents,
            &self.transfer_budget,
            std::time::Duration::from_secs(5),
        );
    }

    #[must_use]
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
    ) -> Option<Self> {
        // NOTE: while it's mutable, it's not part of the hash compute.
        #[allow(clippy::mutable_key_type)]
        let mut seats = HashMap::new();

        let data_device_manager_state =
            DataDeviceManagerState::bind(globals, queue_handle).ok();
        let primary_selection_manager_state =
            PrimarySelectionManagerState::bind(globals, queue_handle).ok();

        // When both globals are not available nothing could be done.
        if data_device_manager_state.is_none()
            && primary_selection_manager_state.is_none()
        {
            return None;
        }

        let seat_state = SeatState::new(globals, queue_handle);
        for seat in seat_state.seats() {
            seats.insert(seat.id(), Default::default());
        }

        Some(Self {
            transfer_budget: Default::default(),
            registry_state: RegistryState::new(globals),
            queue_handle: queue_handle.clone(),
            primary_selection_manager_state,
            primary_sources: Vec::new(),
            data_device_manager_state,
            data_sources: Vec::new(),
            latest_seat: None,
            loop_handle,
            exit: false,
            seat_state,
            seats,
            typed_contents: HashMap::new(),
        })
    }

    /// Store selection for the given target.
    ///
    /// Selection source is only created when `Some(())` is returned.
    pub fn store_selection(
        &mut self,
        ty: SelectionTarget,
        contents: String,
    ) -> Option<()> {
        let data = std::sync::Arc::new(contents.into_bytes());
        self.store_mimes(
            ty,
            ALLOWED_MIME_TYPES
                .iter()
                .map(|mime| ((*mime).to_owned(), data.clone()))
                .collect(),
        )
        .ok()
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.insert(seat.id(), Default::default());
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();

        match capability {
            Capability::Keyboard => {
                seat_state.keyboard = Some(seat.get_keyboard(qh, seat.id()));

                // Selection sources are tied to the keyboard, so add/remove decives
                // when we gain/loss capability.

                if seat_state.data_device.is_none()
                    && self.data_device_manager_state.is_some()
                {
                    seat_state.data_device = self
                        .data_device_manager_state
                        .as_ref()
                        .map(|mgr| mgr.get_data_device(qh, &seat));
                }

                if seat_state.primary_device.is_none()
                    && self.primary_selection_manager_state.is_some()
                {
                    seat_state.primary_device = self
                        .primary_selection_manager_state
                        .as_ref()
                        .map(|mgr| mgr.get_selection_device(qh, &seat));
                }
            }
            Capability::Pointer => {
                seat_state.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            }
            _ => (),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();
        match capability {
            Capability::Keyboard => {
                seat_state.data_device = None;
                seat_state.primary_device = None;

                if let Some(keyboard) = seat_state.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release()
                    }
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = seat_state.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release()
                    }
                }
            }
            _ => (),
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.remove(&seat.id());
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let seat = pointer.data::<PointerData>().unwrap().seat();
        let seat_id = seat.id();
        let seat_state = match self.seats.get_mut(&seat_id) {
            Some(seat_state) => seat_state,
            None => return,
        };

        let mut updated_serial = false;
        for event in events {
            match event.kind {
                PointerEventKind::Press { serial, .. }
                | PointerEventKind::Release { serial, .. } => {
                    updated_serial = true;
                    seat_state.latest_serial = serial;
                }
                _ => (),
            }
        }

        // Only update the seat we're using when the serial got updated.
        if updated_serial {
            self.latest_seat = Some(seat_id);
        }
    }
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
    ) {
    }

    fn drop_performed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
    ) {
    }

    // The selection is finished and ready to be used.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        write_pipe: WritePipe,
    ) {
        self.send_typed(&source.id(), &mime, write_pipe);
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        deleted: &WlDataSource,
    ) {
        self.typed_contents.remove(&deleted.id());
        self.data_sources.retain(|source| source.inner() != deleted)
    }

    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: DndAction,
    ) {
    }

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}

impl PrimarySelectionDeviceHandler for State {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        mime: String,
        write_pipe: WritePipe,
    ) {
        self.send_typed(&source.id(), &mime, write_pipe);
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        deleted: &ZwpPrimarySelectionSourceV1,
    ) {
        self.typed_contents.remove(&deleted.id());
        self.primary_sources
            .retain(|source| source.inner() != deleted)
    }
}

impl Dispatch<WlKeyboard, ObjectId, State> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: <WlKeyboard as sctk::reexports::client::Proxy>::Event,
        data: &ObjectId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use sctk::reexports::client::protocol::wl_keyboard::Event as WlKeyboardEvent;
        let seat_state = match state.seats.get_mut(data) {
            Some(seat_state) => seat_state,
            None => return,
        };
        match event {
            WlKeyboardEvent::Key { serial, .. }
            | WlKeyboardEvent::Modifiers { serial, .. } => {
                seat_state.latest_serial = serial;
                state.latest_seat = Some(data.clone());
            }
            // NOTE both selections rely on keyboard focus.
            WlKeyboardEvent::Enter { serial, .. } => {
                seat_state.latest_serial = serial;
                seat_state.has_focus = true;
            }
            WlKeyboardEvent::Leave { .. } => {
                seat_state.latest_serial = 0;
                seat_state.has_focus = false;
            }
            _ => (),
        }
    }
}

pub struct StoreAck(std::sync::Mutex<Option<Sender<Result<()>>>>);

impl Dispatch<sctk::reexports::client::protocol::wl_callback::WlCallback, StoreAck>
    for State
{
    fn event(
        _: &mut Self,
        _: &sctk::reexports::client::protocol::wl_callback::WlCallback,
        _: sctk::reexports::client::protocol::wl_callback::Event,
        data: &StoreAck,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(reply) = data.0.lock().unwrap().take() {
            let _ = reply.send(Ok(()));
        }
    }
}

delegate_seat!(State);
delegate_pointer!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_registry!(State);

#[derive(Debug, Clone, Copy)]
pub enum SelectionTarget {
    /// The target is clipboard selection.
    Clipboard,
    /// The target is primary selection.
    Primary,
}

#[derive(Debug, Default)]
struct ClipboardSeatState {
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    has_focus: bool,

    /// The latest serial used to set the selection content.
    latest_serial: u32,
}

impl Drop for ClipboardSeatState {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }

        if let Some(pointer) = self.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn retained_sources_bound_empty_publications_and_recover_after_cancellation() {
        let mut retained = HashMap::new();
        retained.insert(1, Vec::new());
        retained.insert(2, Vec::new());
        assert_eq!(
            validate_retained_contents(retained.values(), &Vec::new(), 2, 0)
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        retained.remove(&1);
        validate_retained_contents(retained.values(), &Vec::new(), 2, 0).unwrap();
    }

    #[test]
    fn retained_sources_count_shared_buffers_once_and_charge_spare_capacity() {
        let shared = Arc::new(Vec::with_capacity(8));
        let old = vec![("x/a".into(), shared.clone())];
        let alias = vec![("x/b".into(), shared)];
        validate_retained_contents(std::iter::once(&old), &alias, 2, 8).unwrap();
        let new = vec![("x/c".into(), Arc::new(vec![1]))];
        assert_eq!(
            validate_retained_contents([&old, &alias].into_iter(), &new, 3, 8)
                .unwrap_err()
                .kind(),
            ErrorKind::WouldBlock
        );
        validate_retained_contents(std::iter::empty(), &new, 3, 8).unwrap();
        assert_eq!(
            validate_contents(&old, 7).unwrap_err().kind(),
            ErrorKind::FileTooLarge
        );
    }

    #[test]
    fn publication_budget_counts_aliases_once_and_independent_data_separately() {
        let shared = Arc::new(vec![1, 2, 3]);
        let mut contents = vec![
            ("x/a".to_owned(), shared.clone()),
            ("x/alias".to_owned(), shared),
        ];
        validate_contents(&contents, 3).unwrap();
        contents.push(("x/b".to_owned(), Arc::new(vec![4])));
        assert_eq!(
            validate_contents(&contents, 3).unwrap_err().kind(),
            ErrorKind::FileTooLarge
        );
    }
    #[test]
    fn invalid_or_duplicate_mime_metadata_is_rejected() {
        let data = Arc::new(Vec::new());
        for mime in ["", "x/a\0b", "x/a\nb"] {
            assert_eq!(
                validate_contents(&vec![(mime.to_owned(), data.clone())], 0)
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            validate_contents(
                &vec![("x/a".into(), data.clone()), ("x/a".into(), data)],
                0
            )
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidInput
        );
    }
}
