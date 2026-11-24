//! Imported camera profiles: the user's own DNG Camera Profiles (`.dcp`), chosen like any other
//! profile (`develop.profile {id: "dcp:…"}`). Unlike the built-in looks and LUT profiles, which
//! work on developed colour, a camera profile replaces the raw file's colour model when it is
//! decoded (its matrices and hue/saturation map; see [`lightcraft_raw::dcp`]), so the photo's
//! source is decoded again when it changes. Other photos (JPEG, TIFF…) ignore it.
//!
//! A library on disk keeps a copy of each file in its `Profiles/` folder; the list lives in the
//! library's prefs and the profiles are parsed again when the library opens.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::media::LoadedCameraProfile;
use crate::{Result, Session};

/// Profile id prefix of imported camera profiles.
pub const DCP_PREFIX: &str = "dcp:";
/// Profile browser group of imported camera profiles.
pub const DCP_GROUP: &str = "Camera Profiles";

/// One imported camera profile.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DcpEntry {
    /// `dcp:<slug>`.
    pub id: String,
    /// `ProfileName`, else the file name.
    pub name: String,
    /// The camera it was made for (`UniqueCameraModel`).
    #[serde(default)]
    pub camera_model: String,
    /// The `.dcp` file (the library's copy when there is a library on disk).
    pub file: String,
}

/// Is `id` an imported camera profile id?
pub fn is_dcp_id(id: &str) -> bool {
    id.starts_with(DCP_PREFIX)
}

fn profiles_dir(s: &Session) -> Option<PathBuf> {
    s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Profiles"))
}

/// Parse every imported camera profile again and hand them to the decoder (missing or damaged
/// files are skipped: photos using them develop with the file's own colour, and the profile is not offered or
/// accepted by `develop.profile` until its file is back; the entry is kept).
///
/// A library's own copies are found in `profiles`, the `Profiles/` folder of the library being opened (`None`: not on
/// disk), wherever the saved path points: a library moved or restored elsewhere keeps its profiles (the saved path is
/// updated).
pub fn register_all(s: &mut Session, profiles: Option<&Path>) {
    if let Some(dir) = profiles {
        for p in &mut s.dcp_profiles {
            if let Some(here) = library_copy(dir, &p.file) {
                p.file = here.to_string_lossy().to_string();
            }
        }
    }
    let loaded: Vec<(String, LoadedCameraProfile)> = s
        .dcp_profiles
        .iter()
        .filter_map(|p| {
            lightcraft_raw::dcp::read_dcp(Path::new(&p.file)).ok().and_then(|b| LoadedCameraProfile::parse(&b).ok()).map(|d| (p.id.clone(), d))
        })
        .collect();
    s.media.camera_profiles = loaded.into_iter().collect();
}

/// The copy of saved profile file `file` in the open library's profiles folder `dir`, when it is there under the
/// same name and `file` is not already it.
fn library_copy(dir: &Path, file: &str) -> Option<PathBuf> {
    let saved = Path::new(file);
    let here = dir.join(saved.file_name()?);
    (here != saved && here.is_file()).then_some(here)
}

/// `.dcp` files under `paths` (folders searched).
pub fn dcp_files(paths: &[String]) -> Vec<PathBuf> {
    let is_dcp = |f: &Path| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp"));
    let mut out = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            out.extend(crate::walk::files_in(path, None, crate::walk::Limits::default(), is_dcp).files);
        } else if is_dcp(path) {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Import the `.dcp` files under `paths` → (imported `[{id, name, cameraModel}]`, failed `[[file, error]]`).
/// A profile already imported (same name, camera and bytes) is not added twice.
pub fn import_files(s: &mut Session, paths: &[String]) -> Result<(Vec<Value>, Vec<Value>)> {
    const C: &str = "profile.import";
    let dir = profiles_dir(s);
    if let Some(d) = &dir {
        std::fs::create_dir_all(d).map_err(|e| bad(C, format!("{}: {e}", d.display())))?;
    }
    let (mut imported, mut failed) = (Vec::new(), Vec::new());
    for f in dcp_files(paths) {
        let shown = f.display().to_string();
        let bytes = match lightcraft_raw::dcp::read_dcp(&f) {
            Ok(b) => b,
            Err(e) => {
                failed.push(json!([shown, e.to_string()]));
                continue;
            }
        };
        let loaded = match LoadedCameraProfile::parse(&bytes) {
            Ok(p) => p,
            Err(e) => {
                failed.push(json!([shown, e.to_string()]));
                continue;
            }
        };
        let profile = loaded.profile.clone();
        let stem = f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Camera Profile".into());
        let name = if profile.name.trim().is_empty() { stem.clone() } else { profile.name.trim().to_string() };
        // the same profile imported again: keep the one there is
        let same = s.dcp_profiles.iter().find(|p| {
            p.name == name && p.camera_model == profile.camera_model && lightcraft_raw::dcp::read_dcp(Path::new(&p.file)).is_ok_and(|b| b == bytes)
        });
        if let Some(p) = same {
            imported.push(json!({"id": p.id, "name": p.name, "cameraModel": p.camera_model, "existing": true}));
            continue;
        }
        // (the file name keeps profiles with the same ProfileName apart, e.g. per-body copies)
        let slug = crate::presets::slug(&format!("{}-{stem}", profile.camera_model));
        let mut id = format!("{DCP_PREFIX}{slug}");
        let mut n = 2;
        while s.dcp_profiles.iter().any(|x| x.id == id) {
            id = format!("{DCP_PREFIX}{slug}-{n}");
            n += 1;
        }
        let file = match &dir {
            Some(d) => {
                let to = d.join(format!("{}.dcp", id.trim_start_matches(DCP_PREFIX)));
                // a copy that fails (disk full, no access) fails this file only: the profiles
                // copied before it stay imported, and nothing half-written is left behind
                if let Err(e) = std::fs::write(&to, &bytes) {
                    if to.is_file() {
                        let _ = std::fs::remove_file(&to);
                    }
                    failed.push(json!([shown, format!("{}: {e}", to.display())]));
                    continue;
                }
                to.to_string_lossy().to_string()
            }
            // no library on disk: remember the original (the in-memory session ends with the app)
            None => f.to_string_lossy().to_string(),
        };
        s.media.camera_profiles.insert(id.clone(), loaded);
        s.dcp_profiles.push(DcpEntry { id: id.clone(), name: name.clone(), camera_model: profile.camera_model.clone(), file });
        imported.push(json!({"id": id, "name": name, "cameraModel": profile.camera_model}));
    }
    Ok((imported, failed))
}

/// `{id}` or `{ids: [..]}`: camera profiles taken out of LightCraft. An imported one leaves the library (its copy
/// there is deleted); one of the camera profile folders is hidden ([`crate::dcp_folders::Settings::hidden`]). No file
/// a profile came from is touched. With `ids`, each is tried and the answer says which went and which could not
/// (`{deleted: [id], failed: [[id, error]]}`); the settings are saved once.
fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profile.deleteCamera";
    let Some(ids) = p.get("ids").and_then(Value::as_array) else {
        let id = str_param(p, "id").ok_or_else(|| bad(C, "missing id or ids"))?.to_string();
        if id.starts_with(crate::dcp_folders::ID_PREFIX) {
            if s.media.dcp_folders.snapshot.by_id(&id).is_none() {
                return Err(bad(C, format!("no camera profile `{id}` in the camera profile folders")));
            }
            s.media.dcp_folders.set_hidden(std::slice::from_ref(&id), true).map_err(|e| bad(C, e))?;
        } else {
            delete_one(s, &id)?;
            s.save_prefs()?;
        }
        return Ok(json!({"deleted": id}));
    };
    let (mut deleted, mut failed, mut hide) = (Vec::new(), Vec::new(), Vec::new());
    for id in ids.iter().filter_map(Value::as_str) {
        if id.starts_with(crate::dcp_folders::ID_PREFIX) {
            if s.media.dcp_folders.snapshot.by_id(id).is_some() {
                hide.push(id.to_string());
            } else {
                failed.push(json!([id, format!("no camera profile `{id}` in the camera profile folders")]));
            }
            continue;
        }
        match delete_one(s, id) {
            Ok(()) => deleted.push(json!(id)),
            Err(e) => failed.push(json!([id, e.to_string()])),
        }
    }
    if !deleted.is_empty() {
        s.save_prefs()?;
    }
    if !hide.is_empty() {
        match s.media.dcp_folders.set_hidden(&hide, true) {
            Ok(()) => deleted.extend(hide.iter().map(|id| json!(id))),
            Err(e) => failed.extend(hide.iter().map(|id| json!([id, e]))),
        }
    }
    Ok(json!({"deleted": deleted, "failed": failed}))
}

/// `{ids?}`: profiles of the camera profile folders taken out of LightCraft listed again (all of them without `ids`).
fn show_hidden(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profiles.showHiddenCameraProfiles";
    let ids: Vec<String> = match p.get("ids").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => s.media.dcp_folders.settings.hidden.clone(),
    };
    s.media.dcp_folders.set_hidden(&ids, false).map_err(|e| bad(C, e))?;
    Ok(json!({"shown": ids, "hidden": s.media.dcp_folders.settings.hidden.len()}))
}

/// Take imported camera profile `id` out of the library (its settings not saved yet). Only the library's own copy
/// of the file (made by the import) is deleted, never the file it was imported from.
fn delete_one(s: &mut Session, id: &str) -> Result<()> {
    const C: &str = "profile.deleteCamera";
    let Some(i) = s.dcp_profiles.iter().position(|x| x.id == id) else {
        return Err(bad(C, format!("no imported camera profile `{id}`")));
    };
    // the library's copy goes first; a file outside the library is the user's and stays. A copy that can't be
    // removed (in use, no access) keeps the profile, so the library never holds a file it no longer lists. A path
    // with `..` in it is never the library's copy (it could lead out of the profiles folder).
    if let Some(d) = profiles_dir(s)
        && let Some(entry) = s.dcp_profiles.get(i)
        && Path::new(&entry.file).starts_with(&d)
        && !Path::new(&entry.file).components().any(|c| c == std::path::Component::ParentDir)
    {
        match std::fs::remove_file(&entry.file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(bad(C, format!("could not delete {}: {e}", entry.file))),
        }
    }
    s.dcp_profiles.remove(i);
    s.media.camera_profiles.remove(id);
    s.profile_favorites.retain(|f| f != id);
    s.profile_recent.retain(|f| f != id);
    Ok(())
}

/// One camera's camera profiles: its model as the first profile names it, whether it is `camera`'s (the photo's), and
/// its profiles as (id, name), the default one ([`crate::dcp_folders::Snapshot::default_for`]) first, then by name.
pub type CameraGroup = (String, bool, Vec<(String, String)>);

/// The imported camera profiles that can be chosen (their files loaded), one group per camera: `camera`'s (the photo's)
/// first, then the others by model. A library can hold a thousand of them, so menus show a camera at a time.
pub fn imported_by_camera(s: &Session, camera: Option<&str>) -> Vec<CameraGroup> {
    by_camera(s, camera, false)
}

/// [`imported_by_camera`] with the profiles of the camera profile folders ([`crate::dcp_folders`]) added to their
/// cameras: every profile there is to choose. A folder's profile with the name of one imported for the same camera is
/// left out (it is that one, imported), as is an older file of a name listed already.
pub fn all_by_camera(s: &Session, camera: Option<&str>) -> Vec<CameraGroup> {
    by_camera(s, camera, true)
}

fn by_camera(s: &Session, camera: Option<&str>, folders: bool) -> Vec<CameraGroup> {
    // (camera key, model, profiles, their names folded)
    let mut groups: Vec<(String, String, Vec<(String, String)>, std::collections::HashSet<String>)> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut add = |model: &str, id: String, name: String, from_folder: bool| {
        let key = crate::dcp_folders::camera_key(model);
        let folded = name.trim().to_lowercase();
        match at.get(&key).and_then(|&i| groups.get_mut(i)) {
            Some(g) => {
                if !g.3.insert(folded) && from_folder {
                    return;
                }
                g.2.push((id, name));
            }
            None => {
                at.insert(key.clone(), groups.len());
                groups.push((key, model.trim().to_string(), vec![(id, name)], std::iter::once(folded).collect()));
            }
        }
    };
    for p in s.dcp_profiles.iter().filter(|p| s.media.camera_profiles.contains_key(&p.id)) {
        add(&p.camera_model, p.id.clone(), p.name.clone(), false);
    }
    if folders {
        let snap = &s.media.dcp_folders.snapshot;
        let mut entries: Vec<&crate::dcp_folders::Entry> = snap.shown().collect();
        // the newest file of a name first, so it is the one kept
        entries.sort_by_key(|e| std::cmp::Reverse(e.modified));
        for e in entries {
            add(&e.camera_model, e.id(), e.name.clone(), true);
        }
    }
    let f = &s.media.dcp_folders;
    let mut out: Vec<CameraGroup> = groups
        .into_iter()
        .map(|(_, model, mut list, _)| {
            let first = f.snapshot.default_for(&model, &f.settings.default_name).map(crate::dcp_folders::Entry::id);
            list.sort_by_cached_key(|(id, name)| (Some(id) != first.as_ref(), name.to_lowercase()));
            let mine = camera.is_some_and(|c| crate::dcp_folders::same_camera(c, &model));
            (model, mine, list)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out
}

/// A maker's cameras: (maker, whether it made the photo's camera, its cameras as [`all_by_camera`] gives them).
pub type BrandGroup = (String, bool, Vec<CameraGroup>);

/// [`all_by_camera`] one level up: one group per maker ([`crate::dcp_folders::camera_brand`]), the photo's
/// camera's maker first, then the others by name; in each, the photo's camera first, then by model.
pub fn all_by_brand(s: &Session, camera: Option<&str>) -> Vec<BrandGroup> {
    let mut out: Vec<BrandGroup> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for g in all_by_camera(s, camera) {
        let brand = crate::dcp_folders::camera_brand(&g.0).0;
        let key = brand.to_lowercase();
        let mine = g.1;
        match at.get(&key).and_then(|&i| out.get_mut(i)) {
            Some(b) => {
                b.1 |= mine;
                b.2.push(g);
            }
            None => {
                at.insert(key, out.len());
                out.push((brand, mine, vec![g]));
            }
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out
}

/// The camera profile folders' profiles made for `camera` as (id, name, group), the default one first
/// ([`crate::dcp_folders`]).
pub fn folder_profiles_for(s: &Session, camera: &str) -> Vec<(String, String, String)> {
    let f = &s.media.dcp_folders;
    f.snapshot
        .for_camera(camera, &f.settings.default_name)
        .into_iter()
        .map(|e| (e.id(), e.name.clone(), crate::dcp_folders::GROUP.to_string()))
        .collect()
}

impl Session {
    /// Take the result of a scan of the camera profile folders that has finished: true when the profiles found
    /// changed (renders made before then are made again, their keys hold the profiles' fingerprints).
    pub fn poll_camera_profile_folders(&mut self) -> bool {
        self.media.dcp_folders.poll()
    }
}

/// `{camera?}` → what the camera profile folders' scan found ([`crate::dcp_folders`]).
fn folders_report(s: &mut Session, p: &Value) -> Result<Value> {
    s.poll_camera_profile_folders();
    let i = &s.media.dcp_folders;
    let snap = &i.snapshot;
    let mut out = json!({
        "roots": snap.index.roots,
        "folders": i.settings.folders,
        "automatic": i.settings.automatic,
        "defaultName": i.settings.default_name,
        "scanning": i.scanning(),
        "profiles": snap.index.entries.len(),
        "hidden": snap.hidden_count(),
        "cameras": snap.cameras(),
        "failedCount": snap.index.failed_count,
        "failed": snap.index.failed,
    });
    if let Some(camera) = str_param(p, "camera") {
        let name = i.settings.default_name.as_str();
        let default = snap.default_for(camera, name).map(crate::dcp_folders::Entry::id);
        let list: Vec<Value> = snap
            .for_camera(camera, name)
            .into_iter()
            .map(|e| json!({"id": e.id(), "name": e.name, "cameraModel": e.camera_model, "file": e.path, "default": Some(e.id()) == default}))
            .collect();
        out["camera"] = json!({"name": camera, "profiles": list, "default": default});
    }
    Ok(out)
}

/// Read every camera profile again: the imported ones, and every file in the camera profile folders (not only the
/// changed ones). `wait` walks the folders before it returns; otherwise in the background.
fn reload_all(s: &mut Session, wait: bool) {
    let dir = profiles_dir(s);
    register_all(s, dir.as_deref());
    if wait {
        s.media.dcp_folders.rescan_now(true);
    } else {
        let activity = s.activity.clone();
        s.media.dcp_folders.rescan(&activity, true);
    }
}

fn rescan_folders(s: &mut Session, p: &Value) -> Result<Value> {
    reload_all(s, p.get("wait").and_then(Value::as_bool).unwrap_or(false));
    folders_report(s, &json!({}))
}

fn set_folders(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profiles.setCameraFolders";
    let folders: Vec<String> = p
        .get("folders")
        .and_then(Value::as_array)
        .ok_or_else(|| bad(C, "missing folders: [path, …]"))?
        .iter()
        .filter_map(Value::as_str)
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    let mut kept: Vec<String> = Vec::new();
    for f in folders {
        if !kept.contains(&f) {
            kept.push(f);
        }
    }
    s.media.dcp_folders.settings.folders = kept;
    s.media.dcp_folders.save_settings().map_err(|e| bad(C, e))?;
    reload_all(s, p.get("wait").and_then(Value::as_bool).unwrap_or(false));
    folders_report(s, &json!({}))
}

fn set_automatic(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profiles.setAutomaticCameraProfile";
    let on = p.get("on").and_then(Value::as_bool).ok_or_else(|| bad(C, "missing on: bool"))?;
    s.media.dcp_folders.settings.automatic = on;
    s.media.dcp_folders.save_settings().map_err(|e| bad(C, e))?;
    Ok(json!({"automatic": on}))
}

fn set_default_name(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "profiles.setDefaultCameraProfile";
    let name = p.get("name").and_then(Value::as_str).ok_or_else(|| bad(C, "missing name: string (may be empty)"))?.trim().to_string();
    s.media.dcp_folders.settings.default_name = name.clone();
    s.media.dcp_folders.save_settings().map_err(|e| bad(C, e))?;
    Ok(json!({"defaultName": name}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "profiles.cameraFolders",
            "Camera Profile Folders",
            [],
            None,
            "{camera?} → {roots: [{path, found, files, profiles, truncated}], folders, automatic, defaultName, scanning, profiles, hidden, cameras, failedCount, failed, camera?: {name, default, profiles: [{id, name, cameraModel, file, default}]}}: the camera profiles found in the per-user folder and the folders added (EXPERIMENTAL)",
            always,
            folders_report
        ),
        cmd!(
            query "profiles.rescanCameraFolders",
            "Reload Camera Profiles",
            [],
            None,
            "{wait?: bool} — read every camera profile again: the imported ones and every file in the camera profile folders (in the background unless wait); answers as profiles.cameraFolders",
            always,
            rescan_folders
        ),
        cmd!(
            "profiles.setCameraFolders",
            "Set Camera Profile Folders",
            [],
            None,
            "{folders: [path], wait?: bool} — folders searched for camera profiles besides the per-user one (saved in the config folder); then every camera profile is read again",
            always,
            set_folders
        ),
        cmd!(
            "profiles.setDefaultCameraProfile",
            "Set Default Camera Profiles",
            [],
            None,
            "{name: string} — names separated by commas: of a camera's several profiles in the camera profile folders, a raw with no camera profile chosen develops with the first of these names it has (named so, or holding the name as whole words), else the one whose name the most cameras have; a camera with one profile develops with it",
            always,
            set_default_name
        ),
        cmd!(
            "profiles.setAutomaticCameraProfile",
            "Choose Camera Profiles Automatically",
            [],
            None,
            "{on: bool} — whether a raw with no camera profile chosen develops with a profile of the camera profile folders made for its camera (see profiles.setDefaultCameraProfile)",
            always,
            set_automatic
        ),
        cmd!(
            "profiles.showHiddenCameraProfiles",
            "Show Hidden Camera Profiles",
            [],
            None,
            "{ids?: [dcp:dir-…]} → {shown, hidden}: profiles of the camera profile folders taken out with profile.deleteCamera listed again (all of them without ids)",
            always,
            show_hidden
        ),
        cmd!(
            query "profiles.camera",
            "Imported Camera Profiles",
            [],
            None,
            "{} → {profiles: [{id, name, cameraModel, file}]}: the imported .dcp camera profiles (use with develop.profile)",
            always,
            |s, _| Ok(json!({"profiles": s.dcp_profiles}))
        ),
        cmd!(
            "profile.deleteCamera",
            "Delete Camera Profile",
            [],
            None,
            "{id: dcp:…} or {ids: [dcp:…]} — taken out of LightCraft: an imported one leaves the library (only the library's copy of the file is deleted, never the file it was imported from; photos using it develop with their file's own colour again); one of the camera profile folders (dcp:dir-…) is hidden (profiles.showHiddenCameraProfiles lists it again; its file stays, photos using it keep it). With ids → {deleted: [id], failed: [[id, error]]}",
            always,
            delete
        ),
    ]
}
