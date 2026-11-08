//! Drag-and-drop helpers on top of egui's typed dnd payloads.

use std::any::Any;
use std::sync::Arc;

pub trait ResponseDnd {
    /// `dnd_release_payload`, but leaving a payload of another type alone.
    ///
    /// egui's version takes (and drops) whatever is being dragged before
    /// checking its type, so a drop target for one kind of drag under the
    /// pointer would silently swallow every other kind. Always use this one.
    fn release_payload<T: Any + Send + Sync>(&self) -> Option<Arc<T>>;
}

impl ResponseDnd for egui::Response {
    fn release_payload<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        if egui::DragAndDrop::has_payload_of_type::<T>(&self.ctx) {
            self.dnd_release_payload::<T>()
        } else {
            None
        }
    }
}
