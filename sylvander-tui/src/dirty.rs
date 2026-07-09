//! Dirty flag — mark when AppState has changes that need re-rendering.
//!
//! Single-threaded: wraps `Rc<Cell<bool>>`. Cheap to clone, copy, mark, take.

use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct DirtyFlag(Rc<Cell<bool>>);

impl DirtyFlag {
    pub fn mark(&self) {
        self.0.set(true);
    }

    /// Read and clear in one step. Returns true if there was something to draw.
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }

    pub fn is_set(&self) -> bool {
        self.0.get()
    }
}