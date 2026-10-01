//! Drives `IFileOperation` with one sink per input path.

// Opt in to `unsafe` for this file only; the crate root denies it.
#![allow(unsafe_code)]

use std::collections::HashMap;

use windows_core::{ComObject, HSTRING, Result};

use crate::bindings::{
    CLSCTX_ALL, CoCreateInstance, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
    FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation,
    IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
};
use crate::com::{ComGuard, update_recycle_bin_icon};
use crate::paths::PreparedPath;
use crate::sink::{ItemOutcome, RecycleSink};

/// Recycle every prepared path, returning `(input index, outcome)` pairs.
///
/// `Err` here means COM itself is unusable — nothing was attempted — which the
/// caller maps to exit 3. Per-item problems come back inside the `Ok` vector.
pub fn recycle(items: &[PreparedPath]) -> Result<Vec<(usize, ItemOutcome)>> {
    // Declared first so it drops last: every interface created below releases
    // before CoUninitialize. Nothing COM-typed leaves this function.
    let _com = ComGuard::new()?;

    let mut results: Vec<(usize, ItemOutcome)> = Vec::with_capacity(items.len());
    let mut pending: Vec<&PreparedPath> = items.iter().collect();
    let by_index: HashMap<usize, &PreparedPath> = items.iter().map(|p| (p.index, p)).collect();

    // An error from PreDeleteItem cancels every operation queued behind it
    // (documented behaviour), so after a veto the untouched items are run
    // again in a fresh batch. The vetoed item itself is never retried, so
    // `pending` shrinks on every pass that loops.
    while !pending.is_empty() {
        let batch = run_batch(&pending)?;
        let vetoed = batch.iter().any(|(_, o)| *o == ItemOutcome::Vetoed);

        let mut retry: Vec<&PreparedPath> = Vec::new();
        for (index, outcome) in batch {
            if vetoed && outcome == ItemOutcome::NoResult {
                if let Some(item) = by_index.get(&index) {
                    retry.push(item);
                }
            } else {
                results.push((index, outcome));
            }
        }
        pending = retry;
    }

    if results
        .iter()
        .any(|(_, outcome)| *outcome == ItemOutcome::Recycled)
    {
        update_recycle_bin_icon();
    }

    results.sort_by_key(|(index, _)| *index);
    Ok(results)
}

/// One `IFileOperation` round: queue every item, perform, read the sinks.
fn run_batch(items: &[&PreparedPath]) -> Result<Vec<(usize, ItemOutcome)>> {
    // SAFETY: the caller holds a live `ComGuard`; all arguments are valid.
    let operation: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL.cast_unsigned())? };

    // FOFX_EARLYFAILURE is deliberately omitted: it aborts the whole batch at
    // the first failure, which would defeat per-file reporting. The E_ABORT
    // veto — not this flag set — is what guarantees nothing is destroyed.
    let flags = (FOFX_ADDUNDORECORD
        | FOFX_RECYCLEONDELETE
        | FOF_NOERRORUI
        | FOF_NOCONFIRMATION
        | FOF_SILENT)
        .cast_unsigned();
    // SAFETY: `operation` is a live interface pointer.
    unsafe { operation.SetOperationFlags(flags).ok()? };

    let mut results: Vec<(usize, ItemOutcome)> = Vec::with_capacity(items.len());
    let mut queued: Vec<(usize, ComObject<RecycleSink>)> = Vec::with_capacity(items.len());

    for item in items {
        // SHCreateItemFromParsingName (rather than ILCreateFromPath) returns a
        // Result, so a bad path fails here with its own HRESULT, and the
        // refcounted IShellItem releases on Drop.
        // SAFETY: the HSTRING outlives the call.
        let shell_item: Result<IShellItem> =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(&item.resolved), None) };
        let shell_item = match shell_item {
            Ok(si) => si,
            Err(e) => {
                results.push((item.index, ItemOutcome::Failed(e.code().0)));
                continue;
            }
        };

        // One sink per item: exact 1:1 attribution with no display-name
        // round-tripping, which would be lossy for 8.3 names and junctions.
        let sink = ComObject::new(RecycleSink::default());
        let interface = sink.to_interface::<IFileOperationProgressSink>();
        // SAFETY: both arguments outlive the call; the sink is refcounted and
        // kept alive in `queued` until after PerformOperations.
        if let Err(e) = unsafe { operation.DeleteItem(&shell_item, &interface) }.ok() {
            results.push((item.index, ItemOutcome::Failed(e.code().0)));
            continue;
        }
        queued.push((item.index, sink));
    }

    if !queued.is_empty() {
        // A failure HRESULT here is expected whenever the veto fired; per-item
        // sink state is the source of truth, so the return value is dropped.
        // SAFETY: `operation` is live and every queued item is still alive.
        let _ = unsafe { operation.PerformOperations() };
        results.extend(
            queued
                .iter()
                .map(|(index, sink)| (*index, sink.get().outcome())),
        );
    }

    Ok(results)
}
