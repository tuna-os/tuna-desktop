//! libwacom, as Mutter 51 consults it for tablets and pads
//! (`meta_input_device_get_wacom_device`): the model's name, its pad
//! layout diagram for the pad OSD (`libwacom_get_layout_filename`) and
//! whether it is built into a display (`libwacom_get_integration_flags`,
//! which makes `switch-monitor` skip the span of all monitors).
//!
//! Loaded from the system at run time, like libinput's newer calls in
//! `drm/tablet.rs`: libinput itself depends on libwacom, so every system
//! with tablet support has it, and the build needs no extra package. A
//! system without it treats every tablet as one libwacom does not know,
//! exactly as Mutter built without libwacom does.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

/// What libwacom knows of one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WacomInfo {
    pub name: String,
    pub layout: Option<String>,
    pub integrated: bool,
}

type DatabaseNew = unsafe extern "C" fn() -> *mut c_void;
type DatabaseDestroy = unsafe extern "C" fn(*mut c_void);
type NewFromPath = unsafe extern "C" fn(*const c_void, *const c_char, c_int, *mut c_void) -> *mut c_void;
type GetString = unsafe extern "C" fn(*const c_void) -> *const c_char;
type GetFlags = unsafe extern "C" fn(*const c_void) -> c_int;
type Destroy = unsafe extern "C" fn(*mut c_void);

/// `WFALLBACK_NONE`: an unknown device is no device.
const WFALLBACK_NONE: c_int = 0;

pub struct Libwacom {
    handle: *mut c_void,
    db: *mut c_void,
    database_destroy: DatabaseDestroy,
    new_from_path: NewFromPath,
    get_name: GetString,
    get_layout: GetString,
    get_integration: GetFlags,
    destroy: Destroy,
}

impl Libwacom {
    /// libwacom 2's library and device database, or `None` without them.
    pub fn open() -> Option<Self> {
        // SAFETY: dlopen/dlsym with NUL-terminated names; each symbol is
        // cast to the prototype libwacom.h declares for it.
        unsafe {
            let handle = libc::dlopen(c"libwacom.so.9".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let symbol = |name: &CStr| {
                let ptr = libc::dlsym(handle, name.as_ptr());
                (!ptr.is_null()).then_some(ptr)
            };
            let loaded = (|| {
                let database_new: DatabaseNew = std::mem::transmute(symbol(c"libwacom_database_new")?);
                let database_destroy: DatabaseDestroy = std::mem::transmute(symbol(c"libwacom_database_destroy")?);
                let new_from_path: NewFromPath = std::mem::transmute(symbol(c"libwacom_new_from_path")?);
                let get_name: GetString = std::mem::transmute(symbol(c"libwacom_get_name")?);
                let get_layout: GetString = std::mem::transmute(symbol(c"libwacom_get_layout_filename")?);
                let get_integration: GetFlags = std::mem::transmute(symbol(c"libwacom_get_integration_flags")?);
                let destroy: Destroy = std::mem::transmute(symbol(c"libwacom_destroy")?);
                let db = database_new();
                (!db.is_null()).then_some(Self {
                    handle,
                    db,
                    database_destroy,
                    new_from_path,
                    get_name,
                    get_layout,
                    get_integration,
                    destroy,
                })
            })();
            if loaded.is_none() {
                libc::dlclose(handle);
            }
            loaded
        }
    }

    /// `libwacom_new_from_path (db, devnode, WFALLBACK_NONE, NULL)`.
    pub fn lookup(&self, devnode: &str) -> Option<WacomInfo> {
        let path = CString::new(devnode).ok()?;
        // SAFETY: `db` lives as long as `self`; the device is destroyed
        // before returning and its strings are copied first.
        unsafe {
            let device = (self.new_from_path)(self.db, path.as_ptr(), WFALLBACK_NONE, std::ptr::null_mut());
            if device.is_null() {
                return None;
            }
            let string = |ptr: *const c_char| (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_string_lossy().into_owned());
            let info = WacomInfo {
                name: string((self.get_name)(device)).unwrap_or_default(),
                layout: string((self.get_layout)(device)).filter(|path| !path.is_empty()),
                integrated: (self.get_integration)(device) != 0,
            };
            (self.destroy)(device);
            Some(info)
        }
    }
}

impl Drop for Libwacom {
    fn drop(&mut self) {
        // SAFETY: both were created by `open` and are released once.
        unsafe {
            (self.database_destroy)(self.db);
            libc::dlclose(self.handle);
        }
    }
}
