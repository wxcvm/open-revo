// OpenRevo Declarative WMI Mailbox Channel Provider
// Executes buffer-based ACPI WMI mailbox calls (e.g. AcpiTest_WMI.OemWMIfun)
// Uses a dedicated in-process COM Worker Actor to guarantee 100% thread safety,
// eliminate powershell.exe process spawn overhead, and achieve sub-millisecond execution.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, BSTR, VARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    RPC_C_AUTHN_LEVEL_PKT_PRIVACY, RPC_C_IMP_LEVEL_IMPERSONATE, EOAC_NONE, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{VT_ARRAY, VT_UI1};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
};

const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;

const CANDIDATE_INSTANCES: &[&str] = &[
    r"ACPI\PNP0C14\HWMI_0",
    r"ACPI\PNP0C14\MIFS_0",
    r"ACPI\PNP0C14\MIFS",
    r"ACPI\PNP0C14\0_0",
    r"ACPI\PNP0C14\0",
    r"ACPI\PNP0C14\HWMI",
];

struct MailboxTask {
    namespace: String,
    class_name: String,
    method_name: String,
    in_data: Vec<u8>,
    in_size: usize,
    out_size: usize,
    resp: Sender<Result<Vec<u8>, String>>,
}

struct MailboxWorker {
    tx: Sender<MailboxTask>,
}

static WORKER: LazyLock<Option<MailboxWorker>> = LazyLock::new(init_worker);

fn init_worker() -> Option<MailboxWorker> {
    let (tx, rx) = channel::<MailboxTask>();

    thread::Builder::new()
        .name("wmi-mailbox-actor".to_string())
        .spawn(move || {
            worker_loop(rx);
        })
        .ok()?;

    Some(MailboxWorker { tx })
}

unsafe fn bytes_to_variant(data: &[u8]) -> Option<VARIANT> {
    let sa = SafeArrayCreateVector(VT_UI1, 0, data.len() as u32);
    if sa.is_null() {
        return None;
    }
    let mut ptr = std::ptr::null_mut();
    if SafeArrayAccessData(sa, &mut ptr).is_err() {
        let _ = SafeArrayDestroy(sa);
        return None;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
    let _ = SafeArrayUnaccessData(sa);

    let raw_var = windows::core::imp::VARIANT {
        Anonymous: windows::core::imp::VARIANT_0 {
            Anonymous: windows::core::imp::VARIANT_0_0 {
                vt: (VT_ARRAY.0 | VT_UI1.0) as u16,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: windows::core::imp::VARIANT_0_0_0 {
                    parray: sa as *mut _,
                },
            },
        },
    };
    Some(VARIANT::from_raw(raw_var))
}

unsafe fn variant_to_bytes(var: &VARIANT) -> Option<Vec<u8>> {
    let vt = var.as_raw().Anonymous.Anonymous.vt;
    if (vt & VT_ARRAY.0 as u16) == 0 {
        return None;
    }
    let parray = var.as_raw().Anonymous.Anonymous.Anonymous.parray as *mut SAFEARRAY;
    if parray.is_null() {
        return None;
    }
    let lbound = SafeArrayGetLBound(parray, 1).ok()?;
    let ubound = SafeArrayGetUBound(parray, 1).ok()?;
    if ubound < lbound {
        return Some(Vec::new());
    }
    let count = (ubound - lbound + 1) as usize;
    let mut ptr = std::ptr::null_mut();
    SafeArrayAccessData(parray, &mut ptr).ok()?;
    let slice = std::slice::from_raw_parts(ptr as *const u8, count);
    let result = slice.to_vec();
    let _ = SafeArrayUnaccessData(parray);
    Some(result)
}

fn worker_loop(rx: Receiver<MailboxTask>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let mut locator_opt: Option<IWbemLocator> = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok();
        let mut cached_ns = String::new();
        let mut cached_services: Option<IWbemServices> = None;

        while let Ok(task) = rx.recv() {
            let res = execute_mailbox_task(
                &mut locator_opt,
                &mut cached_ns,
                &mut cached_services,
                &task,
            );
            let _ = task.resp.send(res);
        }
    }
}

unsafe fn get_or_connect_services<'a>(
    locator_opt: &'a mut Option<IWbemLocator>,
    cached_ns: &mut String,
    cached_services: &'a mut Option<IWbemServices>,
    target_ns: &str,
) -> Result<&'a IWbemServices, String> {
    if cached_services.is_some() && cached_ns.eq_ignore_ascii_case(target_ns) {
        return Ok(cached_services.as_ref().unwrap());
    }

    if locator_opt.is_none() {
        *locator_opt = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok();
    }
    let locator = locator_opt.as_ref().ok_or_else(|| "Failed to create WbemLocator".to_string())?;

    let ns_bstr = BSTR::from(target_ns);
    let services = locator
        .ConnectServer(
            &ns_bstr,
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )
        .map_err(|e| format!("ConnectServer to '{}' failed: {}", target_ns, e))?;

    let _ = CoSetProxyBlanket(
        &services,
        RPC_C_AUTHN_WINNT,
        RPC_C_AUTHZ_NONE,
        None,
        RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
        RPC_C_IMP_LEVEL_IMPERSONATE,
        None,
        EOAC_NONE,
    );

    *cached_ns = target_ns.to_string();
    *cached_services = Some(services);
    Ok(cached_services.as_ref().unwrap())
}

unsafe fn query_instance_path(
    services: &IWbemServices,
    class_name: &str,
) -> Result<(String, String), String> {
    let query_lang = BSTR::from("WQL");
    let query_str = BSTR::from(format!("SELECT __RELPATH, InstanceName, Active FROM {}", class_name));

    if let Ok(enumerator) = services.ExecQuery(
        &query_lang,
        &query_str,
        WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
        None,
    ) {
        let mut first_relpath: Option<String> = None;
        let mut matched_relpath: Option<String> = None;

        let mut objects = [None];
        let mut returned = 0;
        while enumerator.Next(500, &mut objects, &mut returned).is_ok() && returned > 0 {
            if let Some(obj) = objects[0].take() {
                let mut rel_val = VARIANT::default();
                let _ = obj.Get(w!("__RELPATH"), 0, &mut rel_val, None, None);
                let rel_str = rel_val.to_string();

                let mut inst_val = VARIANT::default();
                let _ = obj.Get(w!("InstanceName"), 0, &mut inst_val, None, None);
                let inst_str = inst_val.to_string();

                let mut active_val = VARIANT::default();
                let is_active = if obj.Get(w!("Active"), 0, &mut active_val, None, None).is_ok() {
                    bool::try_from(&active_val).unwrap_or(false)
                } else {
                    false
                };

                let target_path = if !rel_str.is_empty() {
                    rel_str
                } else if !inst_str.is_empty() {
                    let escaped = inst_str.replace('\\', "\\\\");
                    format!("{}.InstanceName=\"{}\"", class_name, escaped)
                } else {
                    continue;
                };

                if first_relpath.is_none() {
                    first_relpath = Some(target_path.clone());
                }

                let matches_candidate = CANDIDATE_INSTANCES.iter().any(|cand| inst_str.eq_ignore_ascii_case(cand));
                if is_active && matched_relpath.is_none() {
                    matched_relpath = Some(target_path.clone());
                }
                if matches_candidate {
                    matched_relpath = Some(target_path);
                    break;
                }
            }
        }

        if let Some(path) = matched_relpath.or(first_relpath) {
            return Ok((class_name.to_string(), path));
        }
    }

    // Fallback: Direct candidate probing via GetObject
    for cand in CANDIDATE_INSTANCES {
        let escaped = cand.replace('\\', "\\\\");
        let direct_path = format!("{}.InstanceName=\"{}\"", class_name, escaped);
        let path_bstr = BSTR::from(&direct_path);
        let mut obj_opt: Option<IWbemClassObject> = None;
        if services.GetObject(&path_bstr, WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut obj_opt), None).is_ok() && obj_opt.is_some() {
            return Ok((class_name.to_string(), direct_path));
        }
    }

    Err(format!("No matching WMI instance found for class '{}'", class_name))
}

unsafe fn resolve_instance_path(
    services: &IWbemServices,
    class_name: &str,
) -> Result<(String, String), String> {
    if let Ok(res) = query_instance_path(services, class_name) {
        return Ok(res);
    }

    if !class_name.eq_ignore_ascii_case("AcpiTest_WMI") {
        if let Ok(res) = query_instance_path(services, "AcpiTest_WMI") {
            return Ok(res);
        }
    }

    Err(format!("Cannot bind WMI mailbox instance for class '{}'", class_name))
}

unsafe fn execute_mailbox_task(
    locator_opt: &mut Option<IWbemLocator>,
    cached_ns: &mut String,
    cached_services: &mut Option<IWbemServices>,
    task: &MailboxTask,
) -> Result<Vec<u8>, String> {
    let start = Instant::now();

    let services = get_or_connect_services(locator_opt, cached_ns, cached_services, &task.namespace)?;
    let (actual_class, instance_path) = resolve_instance_path(services, &task.class_name)?;

    let mut padded_in = vec![0u8; task.in_size];
    let copy_len = task.in_data.len().min(task.in_size);
    padded_in[..copy_len].copy_from_slice(&task.in_data[..copy_len]);

    let mut in_var = bytes_to_variant(&padded_in)
        .ok_or_else(|| "Failed to construct input SAFEARRAY VARIANT".to_string())?;

    let class_bstr = BSTR::from(&actual_class);
    let mut class_obj_opt: Option<IWbemClassObject> = None;
    services
        .GetObject(&class_bstr, WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class_obj_opt), None)
        .map_err(|e| format!("GetObject for class '{}' failed: {}", actual_class, e))?;
    let class_obj = class_obj_opt.ok_or_else(|| format!("Class object '{}' is null", actual_class))?;

    let method_bstr = BSTR::from(&task.method_name);
    let mut in_sig_opt: Option<IWbemClassObject> = None;
    class_obj
        .GetMethod(&method_bstr, 0, &mut in_sig_opt, std::ptr::null_mut())
        .map_err(|e| format!("GetMethod for '{}' failed: {}", task.method_name, e))?;
    let in_sig = in_sig_opt.ok_or_else(|| format!("Method '{}' has no input signature", task.method_name))?;

    let in_params = in_sig
        .SpawnInstance(0)
        .map_err(|e| format!("SpawnInstance for input parameters failed: {}", e))?;

    let mut put_ok = in_params.Put(w!("u8Input"), 0, &mut in_var, 0).is_ok();
    if !put_ok {
        put_ok = in_params.Put(w!("InData"), 0, &mut in_var, 0).is_ok();
    }
    if !put_ok {
        put_ok = in_params.Put(w!("Data"), 0, &mut in_var, 0).is_ok();
    }
    if !put_ok {
        return Err(format!("Failed to set input buffer property on method '{}'", task.method_name));
    }

    let inst_path_bstr = BSTR::from(&instance_path);
    let mut out_params_opt: Option<IWbemClassObject> = None;
    services
        .ExecMethod(
            &inst_path_bstr,
            &method_bstr,
            WBEM_GENERIC_FLAG_TYPE(0),
            None,
            &in_params,
            Some(&mut out_params_opt),
            None,
        )
        .map_err(|e| format!("ExecMethod '{}' on '{}' failed: {}", task.method_name, instance_path, e))?;

    let out_params = out_params_opt.ok_or_else(|| "ExecMethod returned null output parameters".to_string())?;

    let mut out_bytes_opt: Option<Vec<u8>> = None;
    let prop_candidates = [w!("u8Output"), w!("OutData"), w!("OutBuf"), w!("Return")];

    for prop in prop_candidates {
        let mut out_val = VARIANT::default();
        if out_params.Get(prop, 0, &mut out_val, None, None).is_ok() {
            if let Some(bytes) = variant_to_bytes(&out_val) {
                if !bytes.is_empty() {
                    out_bytes_opt = Some(bytes);
                    break;
                }
            }
        }
    }

    let mut out_bytes = out_bytes_opt.ok_or_else(|| "Failed to extract output byte array from WMI response".to_string())?;
    if task.out_size > 0 && out_bytes.len() > task.out_size {
        out_bytes.truncate(task.out_size);
    }

    crate::log_debug!(
        "WMI_MAILBOX",
        "WMI call {}.{} executed in {:?}, received {} bytes (target: {})",
        task.class_name,
        task.method_name,
        start.elapsed(),
        out_bytes.len(),
        instance_path
    );

    Ok(out_bytes)
}

/// Invokes a buffer-based WMI method and returns the raw output byte buffer.
/// `in_data`: input byte array (will be padded or clamped to `in_size`).
/// `in_size`: expected input buffer size (e.g. 64 for Xingyao 14).
/// `out_size`: expected output buffer size (e.g. 256 for Xingyao 14).
pub fn invoke_wmi_mailbox(
    namespace: &str,
    class_name: &str,
    method_name: &str,
    in_data: &[u8],
    in_size: usize,
    out_size: usize,
) -> Result<Vec<u8>, String> {
    let worker = WORKER.as_ref().ok_or_else(|| "Mailbox worker not initialized".to_string())?;
    let (resp_tx, resp_rx) = channel();

    worker
        .tx
        .send(MailboxTask {
            namespace: namespace.to_string(),
            class_name: class_name.to_string(),
            method_name: method_name.to_string(),
            in_data: in_data.to_vec(),
            in_size,
            out_size,
            resp: resp_tx,
        })
        .map_err(|e| format!("Failed to send task to mailbox worker: {}", e))?;

    resp_rx
        .recv_timeout(Duration::from_millis(3000))
        .map_err(|e| format!("WMI Mailbox call timed out or worker disconnected: {}", e))?
}

/// Helper to extract integer value from raw output buffer
pub fn extract_value_from_buffer(buf: &[u8], offset: usize, value_type: &str) -> Option<u32> {
    match value_type {
        "u8" => {
            if offset < buf.len() {
                Some(buf[offset] as u32)
            } else {
                None
            }
        }
        "u16_le" => {
            if offset + 1 < buf.len() {
                Some(u16::from_le_bytes([buf[offset], buf[offset + 1]]) as u32)
            } else {
                None
            }
        }
        "u16_be" => {
            if offset + 1 < buf.len() {
                Some(u16::from_be_bytes([buf[offset], buf[offset + 1]]) as u32)
            } else {
                None
            }
        }
        "u32_le" => {
            if offset + 3 < buf.len() {
                Some(u32::from_le_bytes([
                    buf[offset],
                    buf[offset + 1],
                    buf[offset + 2],
                    buf[offset + 3],
                ]))
            } else {
                None
            }
        }
        _ => None,
    }
}
