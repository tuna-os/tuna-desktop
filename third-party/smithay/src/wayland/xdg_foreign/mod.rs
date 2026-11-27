//! Implementation `xdg_foreign` protocol
//!
//! ```rs
//! # extern crate wayland_server;
//! #
//! use wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle};
//! use smithay::{
//!     delegate_xdg_foreign,
//!     wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState}
//! };
//!
//! pub struct State {
//!     xdg_foreign_state: XdgForeignState,
//! }
//!
//! impl XdgForeignHandler for State {
//!     fn xdg_foreignHandler_state(&mut self) -> &mut XdgForeignState {
//!         &mut self.xdg_foreign_state
//!     }
//! }
//!
//! // Delegate xdg foreign handling for State to XdgForeignState.
//! delegate_xdg_foreign!(State);
//!
//! # let mut display = wayland_server::Display::<State>::new().unwrap();
//! # let display_handle = display.handle();
//! let state = State {
//!     xdg_foreign_state: XdgForeignState::new::<State>(&display_handle),
//! };
//! ```

use std::{collections::HashMap, ops::Deref};

use rand::distr::{Alphanumeric, SampleString};
use wayland_protocols::xdg::foreign::{
    zv1::server::{
        zxdg_exporter_v1::ZxdgExporterV1, zxdg_imported_v1::ZxdgImportedV1,
        zxdg_importer_v1::ZxdgImporterV1,
    },
    zv2::server::{
        zxdg_exporter_v2::ZxdgExporterV2, zxdg_imported_v2::ZxdgImportedV2,
        zxdg_importer_v2::ZxdgImporterV2,
    },
};
use wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface, DisplayHandle, GlobalDispatch};

mod handlers;

/// A trait implemented to be notified of activation requests using the xdg foreign protocol.
pub trait XdgForeignHandler: 'static {
    /// Returns the xdg foreign state.
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState;
}

/// The handle contains the unique handle of exported surface.
/// It may be shared with any client, which then can use it to import the surface by calling xdg_importer.import_toplevel.
/// A handle may be used to import the surface multiple times.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct XdgForeignHandle(String);

impl XdgForeignHandle {
    fn new() -> Self {
        Self(Alphanumeric.sample_string(&mut rand::rng(), 32))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for XdgForeignHandle {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// User data of xdg_exported
#[derive(Debug)]
pub struct XdgExportedUserData {
    handle: XdgForeignHandle,
}

/// User data of xdg_imported
#[derive(Debug)]
pub struct XdgImportedUserData {
    handle: XdgForeignHandle,
}

/// An import through either protocol version.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum Imported {
    V1(ZxdgImportedV1),
    V2(ZxdgImportedV2),
}

#[derive(Debug)]
struct ExportedState {
    exported_surface: WlSurface,
    /// Every live import of this export and the child it parents.
    imported_by: HashMap<Imported, Option<WlSurface>>,
}

/// Tracks the list of exported surfaces.
///
/// The zxdg_foreign v1 and v2 globals share one handle table, as in
/// Mutter: a handle exported through either version imports through
/// either (GTK 3 exports with v1, the portal imports with v2).
#[derive(Debug)]
pub struct XdgForeignState {
    exported: HashMap<XdgForeignHandle, ExportedState>,
    exporter: GlobalId,
    importer: GlobalId,
    exporter_v1: GlobalId,
    importer_v1: GlobalId,
}

impl XdgForeignState {
    /// Creates the xdg foreign v1 and v2 globals.
    ///
    /// In order to use this abstraction, your `D` type needs to implement [`XdgForeignHandler`].
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: XdgForeignHandler,
        D: GlobalDispatch<ZxdgExporterV2, ()>,
        D: GlobalDispatch<ZxdgImporterV2, ()>,
        D: GlobalDispatch<ZxdgExporterV1, ()>,
        D: GlobalDispatch<ZxdgImporterV1, ()>,
    {
        let exporter = display.create_global::<D, ZxdgExporterV2, _>(1, ());
        let importer = display.create_global::<D, ZxdgImporterV2, _>(1, ());
        let exporter_v1 = display.create_global::<D, ZxdgExporterV1, _>(1, ());
        let importer_v1 = display.create_global::<D, ZxdgImporterV1, _>(1, ());

        Self {
            exported: HashMap::new(),
            exporter,
            importer,
            exporter_v1,
            importer_v1,
        }
    }

    /// Returns the zxdg_exporter_v1 global.
    pub fn exporter_v1_global(&self) -> GlobalId {
        self.exporter_v1.clone()
    }

    /// Returns the zxdg_importer_v1 global.
    pub fn importer_v1_global(&self) -> GlobalId {
        self.importer_v1.clone()
    }

    /// Returns the xdg_exporter global.
    pub fn exporter_global(&self) -> GlobalId {
        self.exporter.clone()
    }

    /// Returns the xdg_importer global.
    pub fn importer_global(&self) -> GlobalId {
        self.importer.clone()
    }
}

/// Macro to delegate implementation of the xdg foreign to [`XdgForeignState`].
///
/// You must also implement [`XdgForeignHandler`] and
/// [`XdgShellHandler`](crate::wayland::shell::xdg::XdgShellHandler) to use this.
#[macro_export]
macro_rules! delegate_xdg_foreign {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        type __ZxdgExporterV2 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_exporter_v2::ZxdgExporterV2;
        type __ZxdgImporterV2 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_importer_v2::ZxdgImporterV2;

        type __ZxdgExportedV2 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_exported_v2::ZxdgExportedV2;
        type __ZxdgImportedV2 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_imported_v2::ZxdgImportedV2;

        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExporterV2: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImporterV2: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExporterV2: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImporterV2: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExportedV2: $crate::wayland::xdg_foreign::XdgExportedUserData
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImportedV2: $crate::wayland::xdg_foreign::XdgImportedUserData
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );

        type __ZxdgExporterV1 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv1::server::zxdg_exporter_v1::ZxdgExporterV1;
        type __ZxdgImporterV1 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv1::server::zxdg_importer_v1::ZxdgImporterV1;
        type __ZxdgExportedV1 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv1::server::zxdg_exported_v1::ZxdgExportedV1;
        type __ZxdgImportedV1 =
            $crate::reexports::wayland_protocols::xdg::foreign::zv1::server::zxdg_imported_v1::ZxdgImportedV1;

        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExporterV1: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImporterV1: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExporterV1: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImporterV1: ()
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgExportedV1: $crate::wayland::xdg_foreign::XdgExportedUserData
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty:
            [
                __ZxdgImportedV1: $crate::wayland::xdg_foreign::XdgImportedUserData
            ] => $crate::wayland::xdg_foreign::XdgForeignState
        );
    };
}
