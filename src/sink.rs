//! The correctness core.
//!
//! `IFileOperation` with `FOF_ALLOWUNDO` does **not** guarantee recycling. On a
//! volume with the Recycle Bin disabled, on a UNC share, or for a file over the
//! bin quota, the shell silently deletes permanently instead. Nearly every
//! trash tool on Windows carries that latent data-loss bug.
//!
//! This sink closes it: `PreDeleteItem` receives `TSF_DELETE_RECYCLE_IF_POSSIBLE`
//! only when the shell actually intends to recycle. Without that flag we return
//! `E_ABORT` and the item is left alone.
//!
//! Nothing in this file may panic. The `#[implement]` shims are
//! `extern "system"`, so a panic here aborts the whole process mid-operation
//! with no report and no exit code. The lints below make `unwrap`, `expect`,
//! indexing, and explicit panics compile errors; state lives in `Cell`, which
//! cannot panic the way `RefCell` can on a re-entrant borrow.

// The `#[implement]` macro expands to `unsafe` vtable glue; opt in for this
// file only. The crate root denies it.
#![allow(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented
)]

use std::cell::Cell;

use windows_core::{Error, HRESULT, PCWSTR, Ref, Result, implement};

use crate::bindings::{
    E_ABORT, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
    TSF_DELETE_RECYCLE_IF_POSSIBLE,
};

/// What actually happened to one item, as plain data (no COM types escape).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    Recycled,
    /// We vetoed: the shell was about to delete permanently.
    Vetoed,
    Failed(i32),
    /// Queued, but the sink never fired.
    NoResult,
}

#[implement(IFileOperationProgressSink)]
#[derive(Default)]
pub struct RecycleSink {
    vetoed: Cell<bool>,
    post: Cell<Option<HRESULT>>,
}

impl RecycleSink {
    pub fn outcome(&self) -> ItemOutcome {
        if self.vetoed.get() {
            return ItemOutcome::Vetoed;
        }
        match self.post.get() {
            Some(hr) if hr.is_ok() => ItemOutcome::Recycled,
            Some(hr) => ItemOutcome::Failed(hr.0),
            None => ItemOutcome::NoResult,
        }
    }
}

// The generated outer type is what carries the vtable (windows-rs >= 0.59); it
// `Deref`s to `RecycleSink`, so the fields resolve.
impl IFileOperationProgressSink_Impl for RecycleSink_Impl {
    fn StartOperations(&self) -> Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _hrresult: HRESULT) -> Result<()> {
        Ok(())
    }

    fn PreRenameItem(&self, _f: u32, _item: Ref<'_, IShellItem>, _name: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        _dest: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        _dest: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        _dest: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        _dest: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    /// The veto.
    fn PreDeleteItem(&self, dwflags: u32, _item: Ref<'_, IShellItem>) -> Result<()> {
        // `TRANSFER_SOURCE_FLAGS` is generated as a plain integer, so mask
        // by hand.
        if dwflags & TSF_DELETE_RECYCLE_IF_POSSIBLE != 0 {
            return Ok(());
        }
        // The shell is about to delete permanently. Refuse. Per the
        // documentation this also cancels every operation queued after this
        // one; `recycle.rs` re-queues those in a fresh batch.
        self.vetoed.set(true);
        Err(Error::from_hresult(E_ABORT))
    }

    fn PostDeleteItem(
        &self,
        _f: u32,
        _item: Ref<'_, IShellItem>,
        hrdelete: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> Result<()> {
        // Whether PostDeleteItem fires at all after a vetoed PreDeleteItem is
        // undocumented; `vetoed` wins either way, so both behaviours are safe.
        if self.post.get().is_none() {
            self.post.set(Some(hrdelete));
        }
        Ok(())
    }

    fn PreNewItem(&self, _f: u32, _dest: Ref<'_, IShellItem>, _name: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _f: u32,
        _dest: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        _template: &PCWSTR,
        _attrs: u32,
        _hr: HRESULT,
        _new: Ref<'_, IShellItem>,
    ) -> Result<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _total: u32, _sofar: u32) -> Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> Result<()> {
        Ok(())
    }
}
