//! Camera profile folders (EXPERIMENTAL, [`crate::dcp_folders`]): the scan, the camera lookup, the default profile a
//! raw develops with when none is chosen, and the menu's grouping. Every profile is built in memory and written to a
//! temporary folder (no profile file is committed).

use std::path::{Path, PathBuf};

use lightcraft_catalog::PhotoId;
use lightcraft_raw::dcp::{DcpProfile, write_dcp};
use lightcraft_raw::{ColorData, Mat3};
use serde_json::json;

use crate::Session;
use crate::dcp_folders::{DcpFolders, Entry, Index, Snapshot, scan};

/// The name the tests give the default profile (our own; any name works).
const DEFAULT: &str = "Everyday";

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-dcp-folders-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A profile for `camera` named `name`, its matrix scaled by `k` (our own numbers, so profiles differ in their bytes).
fn profile(camera: &str, name: &str, k: f64) -> Vec<u8> {
    let m = Mat3([[0.8 * k, -0.2, 0.0], [-0.3, 1.2, 0.1], [0.0, -0.2, 0.9 * k]]);
    write_dcp(&DcpProfile {
        name: name.into(),
        camera_model: camera.into(),
        color: ColorData { illuminant: [21, 0], color_matrix: [Some(m), None], ..Default::default() },
    })
    .unwrap()
}

/// A folder of profiles: some in nested folders, one damaged file.
fn fill(root: &Path) {
    let (sony, nikon) = (root.join("Camera").join("Sony ILCE-7M4"), root.join("Nikon"));
    std::fs::create_dir_all(&sony).unwrap();
    std::fs::create_dir_all(&nikon).unwrap();
    std::fs::write(sony.join("Sony ILCE-7M4 Neutral.dcp"), profile("Sony ILCE-7M4", "Neutral", 1.0)).unwrap();
    std::fs::write(root.join("Sony ILCE-7M4 Everyday.dcp"), profile("Sony ILCE-7M4", DEFAULT, 1.1)).unwrap();
    std::fs::write(nikon.join("Nikon Z 8 Everyday.dcp"), profile("Nikon Z 8", DEFAULT, 1.2)).unwrap();
    std::fs::write(nikon.join("Broken.dcp"), b"IIRC\x08\0\0\0junk").unwrap();
}

/// Folders searching `root`, with `DEFAULT` as the default profile's name.
fn folders(root: &Path) -> DcpFolders {
    let mut f = DcpFolders::new(vec![root.to_path_buf()], None);
    f.settings.default_name = DEFAULT.into();
    f.rescan_now(false);
    f
}

#[test]
fn a_scan_finds_each_cameras_profiles_reports_damaged_files_and_puts_the_default_one_first() {
    let root = temp_dir("scan");
    fill(&root);
    let index = scan(&[root.clone(), root.join("missing")], &Index::default());
    assert_eq!(index.entries.len(), 3);
    assert_eq!(index.failed_count, 1, "{:?}", index.failed);
    assert!(index.failed[0].path.ends_with("Broken.dcp"));
    assert!(index.roots.iter().any(|r| !r.found), "a folder that isn't there is reported, not skipped silently");
    let snap = Snapshot::new(index);
    assert_eq!(snap.cameras(), 2);
    // the catalog's spellings (Exif make and model joined) find the profiles' own
    let sony = snap.for_camera("SONY ILCE-7M4", DEFAULT);
    assert_eq!(sony.len(), 2);
    assert_eq!(sony[0].name, DEFAULT, "the default one comes first");
    assert_eq!(snap.for_camera("SONY ILCE-7M4", "Neutral")[0].name, "Neutral", "whatever name is the default");
    assert_eq!(snap.default_for("SONY ILCE-7M4", DEFAULT).map(|e| e.name.as_str()), Some(DEFAULT));
    assert!(snap.default_for("NIKON CORPORATION NIKON Z 8", &DEFAULT.to_uppercase()).is_some(), "the name's case is ignored");
    assert_eq!(snap.default_for("SONY ILCE-7M4", "").map(|e| e.name.as_str()), Some(DEFAULT), "of several and no name, the one the Nikon has too");
    // another body of the same make has none
    assert!(snap.for_camera("SONY ILCE-7M3", DEFAULT).is_empty());
    assert!(snap.default_for("Canon EOS R5", DEFAULT).is_none());
    // an id names its entry
    let e = sony[1];
    assert_eq!(snap.by_id(&e.id()).map(|x| x.path.as_str()), Some(e.path.as_str()));
}

#[test]
fn a_rescan_reads_only_changed_files_and_a_fresh_one_reads_them_all() {
    let root = temp_dir("rescan");
    fill(&root);
    let first = scan(std::slice::from_ref(&root), &Index::default());
    // an index that says something else about a file of the same size and time: kept, so the file wasn't read
    let mut previous = first.clone();
    previous.entries[0].name = "Remembered".into();
    let again = scan(std::slice::from_ref(&root), &previous);
    assert!(again.entries.iter().any(|e| e.name == "Remembered"));
    // a fresh rescan (the folders changed in Settings) reads every file again
    let mut f = DcpFolders::new(vec![root.clone()], None);
    f.snapshot = std::sync::Arc::new(Snapshot::new(previous.clone()));
    f.rescan_now(true);
    assert!(f.snapshot.index.entries.iter().all(|e| e.name != "Remembered"));
    // a file that changed is read again
    let changed = PathBuf::from(&first.entries[0].path);
    std::fs::write(&changed, profile("Sony ILCE-7M4", "Changed", 1.7)).unwrap();
    let later = scan(std::slice::from_ref(&root), &previous);
    assert!(later.entries.iter().all(|e| e.name != "Remembered"));
}

#[test]
fn a_raw_without_a_chosen_camera_profile_develops_with_its_cameras_default_one() {
    let root = temp_dir("auto");
    fill(&root);
    let mut s = Session::new().with_fs();
    s.media.dcp_folders = folders(&root);
    let raw = root.join("IMG_0001.dng");
    std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
    let r = s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    let dng = (**s.catalog.photo(id).unwrap()).clone();
    // the same photo as a Sony raw (a format with no colour model of its own to keep)
    let mut arw = dng.clone();
    arw.format = "ARW".into();
    arw.meta.camera = "SONY ILCE-7M4".into();
    let (tag, profile) = s.media.camera_profile_of(&arw).expect("the camera's default profile");
    assert!(tag.starts_with("auto:"), "{tag}");
    assert_eq!(profile.name, DEFAULT);
    let salt = s.media.camera_profile_salt(&arw, &arw.develop.profile.id);
    assert_ne!(salt, 0, "the profile is in the render keys");
    // a camera with no profile in the folders, and a DNG (it has its own), keep their colour
    let mut other = arw.clone();
    other.meta.camera = "SONY ILCE-7M3".into();
    assert!(s.media.camera_profile_of(&other).is_none());
    let mut dng_sony = dng.clone();
    dng_sony.meta.camera = "SONY ILCE-7M4".into();
    assert!(s.media.camera_profile_of(&dng_sony).is_none());
    // no name listed: the one whose name the most cameras have (the Nikon has one too)
    s.execute("profiles.setDefaultCameraProfile", &json!({"name": ""})).unwrap();
    assert_eq!(s.media.camera_profile_of(&arw).map(|(_, p)| p.name.clone()).as_deref(), Some(DEFAULT));
    // another name: that profile, and other render keys
    s.execute("profiles.setDefaultCameraProfile", &json!({"name": "neutral"})).unwrap();
    assert_eq!(s.media.camera_profile_of(&arw).map(|(_, p)| p.name.clone()).as_deref(), Some("Neutral"));
    assert_ne!(s.media.camera_profile_salt(&arw, &arw.develop.profile.id), salt);
    // chosen by its id: it develops with that one, also a DNG
    let neutral = s.media.dcp_folders.snapshot.for_camera("SONY ILCE-7M4", DEFAULT)[1].id();
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    s.execute("develop.profile", &json!({"id": neutral})).unwrap();
    let chosen = s.catalog.photo(id).unwrap();
    let (tag, profile) = s.media.camera_profile_of(chosen).expect("the chosen profile");
    assert!(tag.starts_with(&neutral), "{tag}");
    assert_eq!(profile.name, "Neutral");
    assert_eq!(s.profile_info(&neutral).map(|(n, _)| n), Some("Neutral"));
    // an id that names no profile of the folders is refused
    assert!(s.execute("develop.profile", &json!({"id": "dcp:dir-0000000000000002"})).is_err());
}

#[test]
fn the_report_lists_a_cameras_profiles_with_its_default() {
    let root = temp_dir("report");
    fill(&root);
    let mut s = Session::new();
    s.media.dcp_folders = folders(&root);
    s.execute("profiles.rescanCameraFolders", &json!({"wait": true})).unwrap();
    let r = s.execute("profiles.cameraFolders", &json!({"camera": "SONY ILCE-7M4"})).unwrap();
    assert_eq!(r["profiles"], 3);
    assert_eq!(r["failedCount"], 1);
    assert_eq!(r["defaultName"], DEFAULT);
    let list = r["camera"]["profiles"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|p| p["default"] == true).count(), 1);
    assert_eq!(r["camera"]["default"], list[0]["id"]);
}

#[test]
fn changing_the_folders_reads_every_profile_of_the_new_ones() {
    let (a, b) = (temp_dir("set-a"), temp_dir("set-b"));
    std::fs::write(a.join("one.dcp"), profile("Canon EOS R5", "One", 1.0)).unwrap();
    fill(&b);
    let mut s = Session::new();
    s.media.dcp_folders = folders(&a);
    assert_eq!(s.media.dcp_folders.snapshot.index.entries.len(), 1);
    let r = s.execute("profiles.setCameraFolders", &json!({"folders": [b.to_string_lossy(), b.to_string_lossy()], "wait": true})).unwrap();
    assert_eq!(r["folders"].as_array().map(Vec::len), Some(1), "a folder named twice is kept once");
    assert_eq!(r["profiles"], 4, "the first folder's profile and the added one's: {r}");
    let r = s.execute("profiles.setCameraFolders", &json!({"folders": [], "wait": true})).unwrap();
    assert_eq!(r["profiles"], 1, "a removed folder's profiles are gone");
}

#[test]
fn imported_camera_profiles_group_by_camera_with_the_photos_first() {
    let root = temp_dir("by-camera");
    fill(&root);
    let mut s = Session::new();
    s.media.dcp_folders.settings.default_name = DEFAULT.into();
    s.execute("profile.import", &json!({"paths": [root.to_string_lossy()]})).unwrap();
    let groups = crate::cmd::dcp_profiles::imported_by_camera(&s, Some("NIKON CORPORATION NIKON Z 8"));
    assert_eq!(groups.len(), 2, "one group per camera");
    assert!(groups[0].1 && groups[0].0 == "Nikon Z 8", "the photo's camera first: {groups:?}");
    let sony = &groups[1];
    assert!(!sony.1);
    assert_eq!(sony.2.len(), 2);
    assert_eq!(sony.2[0].1, DEFAULT, "the default one first");
    // no photo camera: by model only
    assert!(crate::cmd::dcp_profiles::imported_by_camera(&s, None).iter().all(|g| !g.1));
    // one level up, by maker: the photo's maker first, each maker's cameras under it
    let brands = crate::cmd::dcp_profiles::all_by_brand(&s, Some("SONY ILCE-7M4"));
    assert_eq!(brands.len(), 2, "one group per maker: {brands:?}");
    assert!(brands[0].1 && brands[0].2[0].0 == "Sony ILCE-7M4", "{brands:?}");
    assert!(!brands[1].1);
    assert_eq!(brands.iter().map(|b| b.2.iter().map(|c| c.2.len()).sum::<usize>()).sum::<usize>(), 3, "every profile is in one");
}

#[test]
fn deleting_imported_profiles_removes_the_librarys_copies_and_never_the_files_they_came_from() {
    let dir = temp_dir("delete");
    let (lib, source) = (dir.join("lib"), dir.join("source"));
    fill(&source);
    let originals: Vec<PathBuf> = crate::cmd::dcp_profiles::dcp_files(&[source.to_string_lossy().to_string()]);
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("profile.import", &json!({"paths": [source.to_string_lossy()]})).unwrap();
    let copies: Vec<PathBuf> = s.dcp_profiles.iter().map(|p| PathBuf::from(&p.file)).collect();
    assert_eq!(copies.len(), 3);
    assert!(copies.iter().all(|c| c.starts_with(&lib) && c.is_file()), "the library holds its own copies");
    // several at once, with one id that names nothing: that one is reported, the others go
    let ids: Vec<String> = s.dcp_profiles.iter().map(|p| p.id.clone()).collect();
    let r = s.execute("profile.deleteCamera", &json!({"ids": [ids[0], ids[1], "dcp:nothing"]})).unwrap();
    assert_eq!(r["deleted"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(s.dcp_profiles.len(), 1);
    // one by one, as before
    s.execute("profile.deleteCamera", &json!({"id": ids[2]})).unwrap();
    assert!(s.dcp_profiles.is_empty());
    assert!(copies.iter().all(|c| !c.exists()), "the library's copies are gone");
    assert!(originals.iter().all(|o| o.is_file()), "the files imported from are still there");
}

#[test]
fn the_menu_lists_folder_profiles_of_every_camera_once_beside_the_imported_ones() {
    let (imported, folder) = (temp_dir("menu-imported"), temp_dir("menu-folder"));
    fill(&folder);
    // one camera's default profile imported (the same file as the folder's), and a camera only imported
    std::fs::write(imported.join("a.dcp"), profile("Sony ILCE-7M4", DEFAULT, 1.1)).unwrap();
    std::fs::write(imported.join("b.dcp"), profile("Canon EOS R5", "Mine", 1.3)).unwrap();
    let mut s = Session::new();
    s.execute("profile.import", &json!({"paths": [imported.to_string_lossy()]})).unwrap();
    s.media.dcp_folders = folders(&folder);
    let all = crate::cmd::dcp_profiles::all_by_camera(&s, None);
    let count = |model: &str| all.iter().find(|g| g.0 == model).map_or(0, |g| g.2.len());
    // cameras that have only folder profiles are listed too, not only the photo's
    assert_eq!(count("Nikon Z 8"), 1, "{all:?}");
    assert_eq!(count("Canon EOS R5"), 1);
    // the imported default profile and the folder's are the same: listed once, with the folder's other one
    assert_eq!(count("Sony ILCE-7M4"), 2, "{all:?}");
    let sony = all.iter().find(|g| g.0 == "Sony ILCE-7M4").unwrap();
    assert!(sony.2[0].0.starts_with("dcp:") && !sony.2[0].0.starts_with("dcp:dir-"), "the imported copy is the one kept");
    // the imported-only list is unchanged
    assert_eq!(crate::cmd::dcp_profiles::imported_by_camera(&s, None).iter().map(|g| g.2.len()).sum::<usize>(), 2);
}

#[test]
fn deleting_a_folders_profile_hides_it_keeps_its_file_and_it_can_be_shown_again() {
    let (dir, config) = (temp_dir("hide"), temp_dir("hide-config"));
    fill(&dir);
    let mut s = Session::new();
    let mut f = DcpFolders::new(vec![dir.clone()], Some(&config));
    f.settings.default_name = DEFAULT.into();
    f.rescan_now(false);
    s.media.dcp_folders = f;
    let sony = s.media.dcp_folders.snapshot.default_for("SONY ILCE-7M4", DEFAULT).unwrap().clone();
    let menu_count = |s: &Session| crate::cmd::dcp_profiles::all_by_camera(s, None).iter().map(|g| g.2.len()).sum::<usize>();
    let before = menu_count(&s);
    let r = s.execute("profile.deleteCamera", &json!({"ids": [sony.id(), "dcp:dir-0000000000000001"]})).unwrap();
    assert_eq!(r["deleted"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(r["failed"].as_array().map(Vec::len), Some(1), "an id that names nothing is reported: {r}");
    assert!(Path::new(&sony.path).is_file(), "the file stays");
    assert!(menu_count(&s) < before, "no longer listed");
    // nor chosen for a raw automatically (the camera's one other profile is, now that it is its only one)
    let now = |f: &DcpFolders| f.snapshot.default_for("SONY ILCE-7M4", DEFAULT).map(|e| e.name.clone());
    assert_eq!(now(&s.media.dcp_folders).as_deref(), Some("Neutral"));
    // a photo developed with it keeps it
    assert!(s.media.dcp_folders.snapshot.by_id(&sony.id()).is_some());
    // kept across sessions
    let again = DcpFolders::new(vec![dir.clone()], Some(&config));
    assert_eq!(now(&again).as_deref(), Some("Neutral"));
    assert_eq!(s.execute("profiles.cameraFolders", &json!({})).unwrap()["hidden"], 1);
    s.execute("profiles.showHiddenCameraProfiles", &json!({})).unwrap();
    assert_eq!(menu_count(&s), before, "listed again");
}

#[test]
fn a_cameras_only_profile_is_its_default_and_of_several_the_first_name_listed_it_has() {
    let dir = temp_dir("names");
    fill(&dir);
    std::fs::write(dir.join("soft.dcp"), profile("Sony ILCE-7M4", "Soft Muted", 1.3)).unwrap();
    std::fs::write(dir.join("plus.dcp"), profile("Sony ILCE-7M4", "Everyday Plus", 1.4)).unwrap();
    let snap = Snapshot::new(scan(&[dir], &Index::default()));
    let pick = |camera: &str, names: &str| snap.default_for(camera, names).map(|e| e.name.clone());
    // one profile: that one, whatever the names
    assert_eq!(pick("NIKON CORPORATION NIKON Z 8", "").as_deref(), Some(DEFAULT));
    assert_eq!(pick("NIKON CORPORATION NIKON Z 8", "Muted").as_deref(), Some(DEFAULT));
    // several: the first name listed that the camera has, as whole words ("Muted" in "Soft Muted")
    assert_eq!(pick("SONY ILCE-7M4", "Muted, Everyday").as_deref(), Some("Soft Muted"));
    assert_eq!(pick("SONY ILCE-7M4", "Vivid; everyday").as_deref(), Some(DEFAULT), "a name it lacks is passed over; exact before words");
    assert_eq!(
        pick("SONY ILCE-7M4", "Mute, Vivid").as_deref(),
        Some(DEFAULT),
        "whole words only (not Soft Muted for Mute); none of the names: the one whose name the most cameras have"
    );
    // the default one is listed first
    assert_eq!(snap.for_camera("SONY ILCE-7M4", "Muted, Everyday")[0].name, "Soft Muted");
}

#[test]
fn of_several_profiles_with_no_name_listed_the_default_is_the_one_whose_name_the_most_cameras_have() {
    let entry = |n: u64, camera: &str, name: &str| Entry {
        path: format!("{n}.dcp"),
        size: 1,
        modified: n,
        fingerprint: n,
        name: name.into(),
        camera_model: camera.into(),
    };
    let entries = vec![
        // three cameras have an everyday profile, two of them a look too, and one a profile of its own
        entry(1, "Make A1", "Base"),
        entry(2, "Make A2", "Base"),
        entry(3, "Make A3", "Base"),
        entry(4, "Make A1", "Look"),
        entry(5, "Make A2", "Look"),
        entry(6, "Make A1", "Own"),
        // a camera whose names no other camera has
        entry(7, "Make B", "One"),
        entry(8, "Make B", "Two"),
        // two names as common as each other
        entry(9, "Make C1", "P"),
        entry(10, "Make C1", "Q"),
        entry(11, "Make C2", "P"),
        entry(12, "Make C2", "Q"),
    ];
    let snap = Snapshot::new(Index { entries, ..Index::default() });
    let pick = |snap: &Snapshot, camera: &str, names: &str| snap.default_for(camera, names).map(|e| e.name.clone());
    assert_eq!(pick(&snap, "Make A1", "").as_deref(), Some("Base"));
    assert_eq!(pick(&snap, "Make A1", "Vivid, Own").as_deref(), Some("Own"), "a name listed comes first");
    assert_eq!(pick(&snap, "Make B", ""), None, "no other camera has its names");
    assert_eq!(pick(&snap, "Make C1", ""), None, "two names as common: none is the everyday one");
    assert_eq!(pick(&snap, "Make C1", "Q").as_deref(), Some("Q"));
    // hidden profiles don't count
    let hidden: Vec<String> = snap.index.entries.iter().filter(|e| e.name == "Base" && e.camera_model != "Make A1").map(Entry::id).collect();
    let snap = Snapshot::hiding(snap.index.clone(), &hidden);
    assert_eq!(pick(&snap, "Make A1", "").as_deref(), Some("Look"));
}

#[test]
fn choosing_camera_profiles_automatically_can_be_turned_off() {
    let root = temp_dir("auto-off");
    fill(&root);
    let mut s = Session::new().with_fs();
    s.media.dcp_folders = folders(&root);
    let file = root.join("IMG_0002.dng");
    std::fs::write(&file, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
    let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let mut raw = (**s.catalog.photo(PhotoId(r["imported"][0].as_u64().unwrap())).unwrap()).clone();
    raw.format = "NEF".into();
    raw.meta.camera = "NIKON CORPORATION NIKON Z 8".into();
    assert!(s.media.auto_camera_profile(&raw).is_some(), "its camera's only profile");
    s.execute("profiles.setAutomaticCameraProfile", &json!({"on": false})).unwrap();
    assert!(s.media.auto_camera_profile(&raw).is_none());
    assert_eq!(s.execute("profiles.cameraFolders", &json!({})).unwrap()["automatic"], false);
}
