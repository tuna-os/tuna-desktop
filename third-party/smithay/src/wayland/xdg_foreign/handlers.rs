use std::collections::HashMap;

use wayland_protocols::xdg::foreign::{
    zv1::server::{
        zxdg_exported_v1::{self, ZxdgExportedV1},
        zxdg_exporter_v1::{self, ZxdgExporterV1},
        zxdg_imported_v1::{self, ZxdgImportedV1},
        zxdg_importer_v1::{self, ZxdgImporterV1},
    },
    zv2::server::{
        zxdg_exported_v2::{self, ZxdgExportedV2},
        zxdg_exporter_v2::{self, ZxdgExporterV2},
        zxdg_imported_v2::{self, ZxdgImportedV2},
        zxdg_importer_v2::{self, ZxdgImporterV2},
    },
};
use wayland_server::{
    backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
    GlobalDispatch, New, Resource,
};

use crate::wayland::{
    compositor,
    shell::{
        is_valid_parent,
        xdg::{XdgShellHandler, XdgToplevelSurfaceData},
    },
};

use super::{
    ExportedState, Imported, XdgExportedUserData, XdgForeignHandle, XdgForeignHandler, XdgForeignState,
    XdgImportedUserData,
};

//
// Export
//

impl<D> GlobalDispatch<ZxdgExporterV2, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgExporterV2, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgExporterV2>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> GlobalDispatch<ZxdgExporterV1, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgExporterV1, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgExporterV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

/// Record a new export under a fresh handle, unique across both
/// protocol versions: either version's importer finds it.
fn export<D: XdgForeignHandler>(state: &mut D, surface: WlSurface) -> XdgForeignHandle {
    let exported = &mut state.xdg_foreign_state().exported;
    let handle = loop {
        let handle = XdgForeignHandle::new();
        if !exported.contains_key(&handle) {
            break handle;
        }
    };
    exported.insert(
        handle.clone(),
        ExportedState {
            exported_surface: surface,
            imported_by: HashMap::new(),
        },
    );
    handle
}

impl<D> Dispatch<ZxdgExporterV2, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgExportedV2, XdgExportedUserData>,
    D: XdgForeignHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZxdgExporterV2,
        request: zxdg_exporter_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_exporter_v2::Request::ExportToplevel { id, surface } => {
                let handle = export(state, surface);
                let exported = data_init.init(
                    id,
                    XdgExportedUserData {
                        handle: handle.clone(),
                    },
                );
                exported.handle(handle.as_str().to_owned());
            }
            zxdg_exporter_v2::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<ZxdgExporterV1, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgExportedV1, XdgExportedUserData>,
    D: XdgForeignHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZxdgExporterV1,
        request: zxdg_exporter_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_exporter_v1::Request::Export { id, surface } => {
                let handle = export(state, surface);
                let exported = data_init.init(
                    id,
                    XdgExportedUserData {
                        handle: handle.clone(),
                    },
                );
                exported.handle(handle.as_str().to_owned());
            }
            zxdg_exporter_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<ZxdgExportedV2, XdgExportedUserData, D> for XdgForeignState
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &ZxdgExportedV2,
        _request: zxdg_exported_v2::Request,
        _data: &XdgExportedUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }

    fn destroyed(state: &mut D, _client: ClientId, _resource: &ZxdgExportedV2, data: &XdgExportedUserData) {
        revoke(state, &data.handle);
    }
}

impl<D> Dispatch<ZxdgExportedV1, XdgExportedUserData, D> for XdgForeignState
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &ZxdgExportedV1,
        _request: zxdg_exported_v1::Request,
        _data: &XdgExportedUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }

    fn destroyed(state: &mut D, _client: ClientId, _resource: &ZxdgExportedV1, data: &XdgExportedUserData) {
        revoke(state, &data.handle);
    }
}

/// Revoke an export: every importer is told its import is destroyed and
/// every child it parented loses that parent (Mutter's
/// `meta_wayland_xdg_exported_destroy`).
fn revoke<D>(state: &mut D, handle: &XdgForeignHandle)
where
    D: XdgForeignHandler + XdgShellHandler,
{
    let Some(exported) = state.xdg_foreign_state().exported.remove(handle) else {
        return;
    };
    for (imported, child) in exported.imported_by {
        imported.send_destroyed();
        if let Some(child) = child {
            unparent(state, &child, &exported.exported_surface);
        }
    }
}

//
// Import
//

impl<D> GlobalDispatch<ZxdgImporterV2, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgImporterV2, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgImporterV2>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> GlobalDispatch<ZxdgImporterV1, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgImporterV1, ()>,
{
    fn bind(
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgImporterV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

/// Track a new import of `handle`; an unknown handle is destroyed at
/// once, as the protocol asks.
fn import<D: XdgForeignHandler>(state: &mut D, imported: Imported) {
    let data = imported.data();
    match state.xdg_foreign_state().exported.get_mut(&data.handle) {
        Some(exported) => {
            exported.imported_by.insert(imported, None);
        }
        None => imported.send_destroyed(),
    }
}

impl<D: XdgForeignHandler> Dispatch<ZxdgImporterV2, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgImportedV2, XdgImportedUserData>,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZxdgImporterV2,
        request: zxdg_importer_v2::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_importer_v2::Request::ImportToplevel { id, handle } => {
                let imported = data_init.init(
                    id,
                    XdgImportedUserData {
                        handle: XdgForeignHandle(handle),
                    },
                );
                import(state, Imported::V2(imported));
            }
            zxdg_importer_v2::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D: XdgForeignHandler> Dispatch<ZxdgImporterV1, (), D> for XdgForeignState
where
    D: Dispatch<ZxdgImportedV1, XdgImportedUserData>,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZxdgImporterV1,
        request: zxdg_importer_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_importer_v1::Request::Import { id, handle } => {
                let imported = data_init.init(
                    id,
                    XdgImportedUserData {
                        handle: XdgForeignHandle(handle),
                    },
                );
                import(state, Imported::V1(imported));
            }
            zxdg_importer_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<ZxdgImportedV2, XdgImportedUserData, D> for XdgForeignState
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZxdgImportedV2,
        request: zxdg_imported_v2::Request,
        _data: &XdgImportedUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_imported_v2::Request::SetParentOf { surface } => {
                set_parent_of(state, Imported::V2(resource.clone()), surface)
            }
            zxdg_imported_v2::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, resource: &ZxdgImportedV2, _data: &XdgImportedUserData) {
        drop_import(state, &Imported::V2(resource.clone()));
    }
}

impl<D> Dispatch<ZxdgImportedV1, XdgImportedUserData, D> for XdgForeignState
where
    D: XdgForeignHandler + XdgShellHandler,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZxdgImportedV1,
        request: zxdg_imported_v1::Request,
        _data: &XdgImportedUserData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            zxdg_imported_v1::Request::SetParentOf { surface } => {
                set_parent_of(state, Imported::V1(resource.clone()), surface)
            }
            zxdg_imported_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut D, _client: ClientId, resource: &ZxdgImportedV1, _data: &XdgImportedUserData) {
        drop_import(state, &Imported::V1(resource.clone()));
    }
}

/// Make the exported surface `child`'s parent. Each import parents at
/// most one child at a time (Mutter's `imported->parent_of`): a second
/// call moves the relationship.
fn set_parent_of<D>(state: &mut D, imported: Imported, child: WlSurface)
where
    D: XdgForeignHandler + XdgShellHandler,
{
    let handle = imported.data().handle.clone();
    let Some(exported) = state.xdg_foreign_state().exported.get(&handle) else {
        // Revoked: the import is inert until the client destroys it.
        return;
    };
    let Some(previous) = exported.imported_by.get(&imported).cloned() else {
        return;
    };
    let parent = exported.exported_surface.clone();

    let mut invalid = false;
    let mut changed = false;
    compositor::with_states(&child, |states| {
        if let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() {
            if is_valid_parent(&child, &parent) {
                let mut role = data.lock().unwrap();
                changed = role.parent.as_ref() != Some(&parent);
                role.parent = Some(parent.clone());
            } else {
                invalid = true;
            }
        }
    });

    if invalid {
        imported.post_invalid_surface("invalid parent relationship");
        return;
    }

    if let Some(exported) = state.xdg_foreign_state().exported.get_mut(&handle) {
        exported.imported_by.insert(imported, Some(child.clone()));
    }
    if let Some(previous) = previous.filter(|previous| *previous != child) {
        unparent(state, &previous, &parent);
    }
    if changed {
        parent_changed(state, &child);
    }
}

/// An import was destroyed: the child it parented loses that parent.
fn drop_import<D>(state: &mut D, imported: &Imported)
where
    D: XdgForeignHandler + XdgShellHandler,
{
    let handle = &imported.data().handle;
    let Some(exported) = state.xdg_foreign_state().exported.get_mut(handle) else {
        return;
    };
    let parent = exported.exported_surface.clone();
    if let Some(Some(child)) = exported.imported_by.remove(imported) {
        unparent(state, &child, &parent);
    }
}

/// Clear `child`'s parent if it is still `parent`.
fn unparent<D>(state: &mut D, child: &WlSurface, parent: &WlSurface)
where
    D: XdgShellHandler,
{
    let mut changed = false;
    compositor::with_states(child, |states| {
        let Some(data) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return;
        };
        let data = &mut *data.lock().unwrap();
        if data.parent.as_ref() == Some(parent) {
            data.parent = None;
            changed = true;
        }
    });
    if changed {
        parent_changed(state, child);
    }
}

fn parent_changed<D: XdgShellHandler>(state: &mut D, child: &WlSurface) {
    if let Some(toplevel) = state
        .xdg_shell_state()
        .toplevel_surfaces()
        .iter()
        .find(|toplevel| toplevel.wl_surface() == child)
        .cloned()
    {
        XdgShellHandler::parent_changed(state, toplevel);
    }
}

impl Imported {
    fn data(&self) -> &XdgImportedUserData {
        match self {
            Imported::V1(imported) => imported
                .data::<XdgImportedUserData>()
                .expect("xdg_imported user data"),
            Imported::V2(imported) => imported
                .data::<XdgImportedUserData>()
                .expect("xdg_imported user data"),
        }
    }

    fn send_destroyed(&self) {
        match self {
            Imported::V1(imported) => imported.destroyed(),
            Imported::V2(imported) => imported.destroyed(),
        }
    }

    fn post_invalid_surface(&self, message: &str) {
        match self {
            // zxdg_imported_v1 has no error enum; Mutter posts
            // WL_DISPLAY_ERROR_INVALID_OBJECT on the import.
            Imported::V1(imported) => imported.post_error(0u32, message.to_owned()),
            Imported::V2(imported) => {
                imported.post_error(zxdg_imported_v2::Error::InvalidSurface, message.to_owned())
            }
        }
    }
}
