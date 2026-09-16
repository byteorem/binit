//! Generates the Win32 bindings binit needs into `OUT_DIR`.
//!
//! Only the listed items are projected, so the dependency graph stays small
//! and every API the binary touches is enumerated in one place. The names are
//! bare because windows-bindgen 0.100 resolves bare names across namespaces;
//! each of these is unique within the Win32 metadata.

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let out = format!("{out_dir}/bindings.rs");

    let filter = [
        // COM apartment and activation (com.rs, recycle.rs)
        "CoInitializeEx",
        "CoUninitialize",
        "CoCreateInstance",
        "COINIT_APARTMENTTHREADED",
        "COINIT_DISABLE_OLE1DDE",
        "CLSCTX_ALL",
        // The file operation and its sink (recycle.rs, sink.rs)
        "FileOperation",
        "IFileOperation",
        "IFileOperationProgressSink",
        "IShellItem",
        "SHCreateItemFromParsingName",
        "TSF_DELETE_RECYCLE_IF_POSSIBLE",
        "FOFX_ADDUNDORECORD",
        "FOFX_RECYCLEONDELETE",
        "FOF_NOERRORUI",
        "FOF_NOCONFIRMATION",
        "FOF_SILENT",
        // Recycle Bin icon refresh (com.rs)
        "GetModuleHandleW",
        "GetProcAddress",
        // subst resolution (paths.rs)
        "QueryDosDeviceW",
        // Error taxonomy (sink.rs, report.rs)
        "E_ABORT",
        "E_ACCESSDENIED",
        "ERROR_FILE_NOT_FOUND",
        "ERROR_PATH_NOT_FOUND",
        "ERROR_SHARING_VIOLATION",
    ];

    let mut args = vec!["--in", "default", "--out", &out, "--flat", "--filter"];
    args.extend(filter);
    windows_bindgen::bindgen(args);

    println!("cargo:rerun-if-changed=build.rs");
}
