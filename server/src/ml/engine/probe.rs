//! Is there an NVIDIA GPU this container can use? Asks the driver (`libcuda.so.1`, which
//! the NVIDIA container runtime mounts from the host) without creating a context. Runs in
//! a short-lived child process (`hyperfocal ml-engine probe`), never in the server.

use serde::{Deserialize, Serialize};
use std::ffi::{CStr, c_char, c_int, c_void};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Gpus {
    /// CUDA driver version (e.g. 13000 for CUDA 13.0, drivers 580 and later).
    pub driver: i32,
    pub devices: Vec<Gpu>,
    /// Why none could be used.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub memory_mb: u64,
}

type CuInit = unsafe extern "C" fn(u32) -> c_int;
type CuDriverGetVersion = unsafe extern "C" fn(*mut c_int) -> c_int;
type CuDeviceGetCount = unsafe extern "C" fn(*mut c_int) -> c_int;
type CuDeviceGet = unsafe extern "C" fn(*mut c_int, c_int) -> c_int;
type CuDeviceGetName = unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int;
type CuDeviceTotalMem = unsafe extern "C" fn(*mut usize, c_int) -> c_int;

fn sym(lib: *mut c_void, name: &CStr) -> Option<*mut c_void> {
    // SAFETY: lib is a live handle from dlopen; name is NUL-terminated.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

/// What the CUDA driver reports.
pub fn probe() -> Gpus {
    let mut g = Gpus::default();
    // SAFETY: dlopen with a constant name; the handle is never closed (the process exits).
    let lib = unsafe { libc::dlopen(c"libcuda.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if lib.is_null() {
        g.error = Some("no NVIDIA driver in the container (libcuda.so.1 is missing: is the container started with --runtime=nvidia?)".into());
        return g;
    }
    let (Some(init), Some(ver), Some(count), Some(get), Some(name), Some(mem)) = (
        sym(lib, c"cuInit"),
        sym(lib, c"cuDriverGetVersion"),
        sym(lib, c"cuDeviceGetCount"),
        sym(lib, c"cuDeviceGet"),
        sym(lib, c"cuDeviceGetName"),
        sym(lib, c"cuDeviceTotalMem_v2"),
    ) else {
        g.error = Some("the NVIDIA driver library is incomplete".into());
        return g;
    };
    // SAFETY: the symbols have these C signatures (CUDA driver API); every out-pointer
    // points at a live local.
    unsafe {
        let init: CuInit = std::mem::transmute(init);
        let ver: CuDriverGetVersion = std::mem::transmute(ver);
        let count: CuDeviceGetCount = std::mem::transmute(count);
        let get: CuDeviceGet = std::mem::transmute(get);
        let name: CuDeviceGetName = std::mem::transmute(name);
        let mem: CuDeviceTotalMem = std::mem::transmute(mem);
        let mut v = 0;
        if ver(&mut v) == 0 {
            g.driver = v;
        }
        let rc = init(0);
        if rc != 0 {
            g.error = Some(format!("the NVIDIA driver could not start (CUDA error {rc}{})", if rc == 100 { ": no GPU visible, check NVIDIA_VISIBLE_DEVICES" } else { "" }));
            return g;
        }
        let mut n = 0;
        if count(&mut n) != 0 || n <= 0 {
            g.error = Some("no NVIDIA GPU visible (check NVIDIA_VISIBLE_DEVICES)".into());
            return g;
        }
        for i in 0..n {
            let mut dev = 0;
            if get(&mut dev, i) != 0 {
                continue;
            }
            let mut buf = [0 as c_char; 256];
            let nm = if name(buf.as_mut_ptr(), buf.len() as c_int, dev) == 0 { CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned() } else { format!("GPU {i}") };
            let mut bytes = 0usize;
            let _ = mem(&mut bytes, dev);
            g.devices.push(Gpu { name: nm, memory_mb: (bytes >> 20) as u64 });
        }
    }
    g
}
