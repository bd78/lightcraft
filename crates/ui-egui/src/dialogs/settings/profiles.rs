//! Settings ▸ Profiles: camera profiles (`.dcp`) imported into the library — from files or a whole folder — and the
//! camera profile folders (EXPERIMENTAL, `lightcraft_engine::dcp_folders`): the user's own folder and the ones they
//! add, used where they are, and the name of the profile a raw develops with when none is chosen. Everything goes
//! through commands (`profile.import`, `profile.deleteCamera`, `profiles.*`).

use serde_json::{Value, json};

use super::widgets::{check, field, heading, hint, path_label, row};
use super::{LightcraftApp, RichText, Tokens, register};

/// Where the last import's summary is kept between frames (egui temporary memory).
fn news_id() -> egui::Id {
    egui::Id::unique("settings:cameraProfileNews")
}

/// The camera profiles' grid's height at most before it scrolls.
const LIST_HEIGHT: f32 = 300.0;

/// One camera profile of the grid: imported, or found in a camera profile folder.
struct GridRow {
    id: String,
    brand: String,
    model: String,
    name: String,
    /// The camera as the profile names it.
    camera: String,
    /// "Imported", or the folder it was found in (its last name).
    source: String,
    /// The profile's file.
    file: String,
}

/// The grid's rows, by brand, model and name, and their ids; made again when the profiles change (thousands of them:
/// not every frame).
struct GridRows {
    key: u64,
    rows: Vec<GridRow>,
    ids: std::collections::HashSet<String>,
}

fn grid_rows_id() -> egui::Id {
    egui::Id::unique("settings:cameraProfileGridRows")
}

/// What tells the profiles listed changed: the imported ones and the folders' index (a new one on every scan and every
/// profile hidden or shown again).
fn grid_key(app: &LightcraftApp) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in &app.session.dcp_profiles {
        p.id.hash(&mut h);
    }
    let snap = &app.session.media.dcp_folders.snapshot;
    (std::sync::Arc::as_ptr(snap) as usize).hash(&mut h);
    snap.index.entries.len().hash(&mut h);
    app.session.media.dcp_folders.settings.hidden.len().hash(&mut h);
    h.finish()
}

fn grid_rows(app: &LightcraftApp) -> GridRows {
    let imported = crate::i18n::tr("Imported").to_string();
    let mut rows: Vec<GridRow> = Vec::new();
    let mut add = |id: String, name: &str, camera: &str, source: String, file: &str| {
        let (brand, model) = lightcraft_engine::dcp_folders::camera_brand(camera);
        rows.push(GridRow { id, brand, model, name: name.to_string(), camera: camera.to_string(), source, file: file.to_string() });
    };
    for p in &app.session.dcp_profiles {
        add(p.id.clone(), &p.name, &p.camera_model, imported.clone(), &p.file);
    }
    let snap = &app.session.media.dcp_folders.snapshot;
    for e in snap.shown() {
        let source = snap
            .root_of(e)
            .map(|r| std::path::Path::new(&r.path).file_name().map_or_else(|| r.path.clone(), |n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        add(e.id(), &e.name, &e.camera_model, source, &e.path);
    }
    rows.sort_by_cached_key(|r| (r.brand.to_lowercase(), r.model.to_lowercase(), r.name.to_lowercase(), r.source.to_lowercase()));
    let ids = rows.iter().map(|r| r.id.clone()).collect();
    GridRows { key: grid_key(app), rows, ids }
}

/// What the camera profiles' grid shows and has selected, kept between frames (egui temporary memory).
#[derive(Clone, Default)]
struct GridState {
    /// The brand shown (empty: every brand).
    brand: String,
    /// The model shown, of that brand (empty: every model).
    model: String,
    /// Text the profile's name holds (empty: any).
    name: String,
    /// The profiles ticked, by id.
    selected: std::collections::HashSet<String>,
}

fn grid_state_id() -> egui::Id {
    egui::Id::unique("settings:cameraProfileGrid")
}

/// A checkbox without a visible label, named `name` for screen readers (its state with it).
fn named_check(ui: &mut egui::Ui, id: String, on: &mut bool, name: String) -> bool {
    let r = ui.checkbox(on, "");
    let state = *on;
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, r.enabled(), state, name.clone()));
    register(ui, id, r.rect);
    r.changed()
}

/// `file.importCameraProfiles` (`folder`: `file.importCameraProfileFolder`): `.dcp` files, or every `.dcp` under a
/// folder, imported into the library (`profile.import`); asks for them when `paths` / `path` is missing. The summary
/// is toasted and shown in Settings ▸ Profiles.
pub(crate) fn import_camera_profiles_command(
    app: &mut LightcraftApp,
    ctx: &egui::Context,
    id: &str,
    p: &Value,
    folder: bool,
) -> Result<Value, String> {
    let given: Option<Vec<String>> = if folder {
        p.get("path").and_then(Value::as_str).map(|s| vec![s.to_string()])
    } else {
        p.get("paths").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
    };
    let paths = match given {
        Some(paths) => paths,
        None => {
            let (key, request) = if folder {
                ("path", crate::pick::PickRequest::folder(crate::i18n::tr("Import Folder of Camera Profiles")))
            } else {
                (
                    "paths",
                    crate::pick::PickRequest::files(
                        Some(crate::i18n::tr("Import Camera Profiles").into()),
                        crate::i18n::tr("Camera Profiles"),
                        &["dcp"],
                    ),
                )
            };
            let picked = crate::pick::ask(app, id, p, key, request, |s| {
                if folder { s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x]) } else { s.pick_preset_files.as_mut().map(|f| f()) }
            });
            match picked {
                crate::pick::Picked::Now(paths) => paths,
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err(crate::i18n::tr("No file dialog on this platform").to_string()),
            }
        }
    };
    if paths.is_empty() {
        return Ok(Value::Null);
    }
    // only the camera profiles of a folder (`profile.import` would take its LUTs too)
    let files: Vec<String> = lightcraft_engine::cmd::dcp_profiles::dcp_files(&paths).iter().map(|f| f.to_string_lossy().to_string()).collect();
    let r = if files.is_empty() { json!({"imported": [], "failed": []}) } else { app.run("profile.import", json!({"paths": files}))? };
    let list = r.get("imported").and_then(Value::as_array).cloned().unwrap_or_default();
    let imported = list.len();
    let existing = list.iter().filter(|v| v.get("existing").and_then(Value::as_bool) == Some(true)).count();
    let failed: Vec<String> = r
        .get("failed")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|f| Some(format!("{}: {}", f.get(0)?.as_str()?, f.get(1)?.as_str()?))).collect())
        .unwrap_or_default();
    let mut text = crate::i18n::tr_format!("{n} camera profiles imported ({m} already there)", n = imported - existing, m = existing);
    if !failed.is_empty() {
        text += &crate::i18n::tr_format!(" · {n} could not be read", n = failed.len());
    } else if imported == 0 {
        text = crate::i18n::tr("No camera profiles (.dcp) found there").to_string();
    }
    ctx.data_mut(|d| d.insert_temp(news_id(), (text.clone(), failed.clone())));
    if failed.is_empty() {
        app.toast(ctx, text)
    } else {
        app.toast_error(ctx, text)
    }
    Ok(r)
}

/// `file.addCameraProfileFolder` `{path?}`: a folder searched for camera profiles, added to the ones chosen
/// (`profiles.setCameraFolders`, which reads every camera profile again); asks for it when `path` is missing.
pub(crate) fn add_camera_profile_folder_command(app: &mut LightcraftApp, id: &str, p: &Value) -> Result<Value, String> {
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let request = crate::pick::PickRequest::folder(crate::i18n::tr("Add Camera Profile Folder"));
            match crate::pick::ask(app, id, p, "path", request, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err(crate::i18n::tr("No folder dialog on this platform").to_string()),
            }
        }
    };
    let mut folders = app.session.media.dcp_folders.settings.folders.clone();
    if !folders.contains(&path) {
        folders.push(path);
    }
    app.run("profiles.setCameraFolders", json!({"folders": folders}))
}

/// Settings ▸ Profiles.
pub(super) fn profiles_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    imported_section(app, ui, t);
    ui.add_space(10.0);
    folders_section(app, ui, t);
}

fn run_or_toast(app: &mut LightcraftApp, ui: &egui::Ui, command: &str, p: Value) {
    if let Err(e) = app.run(command, p) {
        app.toast_error(ui.ctx(), e);
    }
}

fn imported_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "Camera profiles");
    hint(
        ui,
        t,
        "Every camera profile (.dcp) LightCraft knows: the ones you import, copied into the library's Profiles folder, and the ones in the camera profile folders below, used where they are. Deleting one takes it out of LightCraft (an imported one's copy goes, one from a folder is hidden): the file it came from always stays.",
    );
    row(ui, t, "Import", |ui| {
        let b = ui.button(crate::i18n::tr("Import .dcp Files…"));
        register(ui, "button:importCameraProfiles", b.rect);
        if b.clicked() {
            run_or_toast(app, ui, "file.importCameraProfiles", json!({}));
        }
        let b = ui.button(crate::i18n::tr("Import Folder…"));
        register(ui, "button:importCameraProfileFolder", b.rect);
        if b.clicked() {
            run_or_toast(app, ui, "file.importCameraProfileFolder", json!({}));
        }
    });
    if let Some((text, failed)) = ui.data(|d| d.get_temp::<(String, Vec<String>)>(news_id())) {
        let r = ui.label(RichText::new(text).size(11.5).color(if failed.is_empty() { t.text_label } else { t.caution }));
        register(ui, "settings:cameraProfileNews", r.rect);
        for f in failed.iter().take(5) {
            ui.label(RichText::new(f).size(11.0).color(t.text_dim));
        }
        if failed.len() > 5 {
            ui.label(RichText::new(crate::i18n::tr_format!("…and {n} more", n = failed.len() - 5)).size(11.0).color(t.text_dim));
        }
    }
    let hidden = app.session.media.dcp_folders.snapshot.hidden_count();
    let mut show_hidden = false;
    let key = grid_key(app);
    let grid = match ui.data(|d| d.get_temp::<std::sync::Arc<GridRows>>(grid_rows_id())).filter(|g| g.key == key) {
        Some(g) => g,
        None => {
            let g = std::sync::Arc::new(grid_rows(app));
            ui.data_mut(|d| d.insert_temp(grid_rows_id(), g.clone()));
            g
        }
    };
    let rows = &grid.rows;
    if rows.is_empty() {
        hint(ui, t, "No camera profiles: import some, or add a folder that holds them.");
    }
    let mut st: GridState = ui.data_mut(|d| d.get_temp::<GridState>(grid_state_id())).unwrap_or_default();
    // ticks of profiles no longer listed go
    st.selected.retain(|id| grid.ids.contains(id));
    let mut delete: Option<Vec<String>> = None;
    if !rows.is_empty() {
        let mut brands: Vec<&str> = rows.iter().map(|r| r.brand.as_str()).collect();
        brands.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if !st.brand.is_empty() && !brands.iter().any(|b| b.eq_ignore_ascii_case(&st.brand)) {
            st.brand.clear();
        }
        let mut models: Vec<&str> = rows.iter().filter(|r| r.brand.eq_ignore_ascii_case(&st.brand)).map(|r| r.model.as_str()).collect();
        models.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if !st.model.is_empty() && !models.iter().any(|m| m.eq_ignore_ascii_case(&st.model)) {
            st.model.clear();
        }
        row(ui, t, "Filter", |ui| {
            let all_brands = crate::i18n::tr("All brands").to_string();
            let shown = if st.brand.is_empty() { all_brands.clone() } else { st.brand.clone() };
            let r = crate::widgets::combo_box("settings:cameraProfileBrand").width(150.0).selected_text(shown.clone()).show_ui(ui, |ui| {
                let r = ui.selectable_label(st.brand.is_empty(), &all_brands);
                register(ui, "button:cameraProfileBrand-all", r.rect);
                if r.clicked() {
                    st.brand.clear();
                    st.model.clear();
                }
                for b in &brands {
                    let r = ui.selectable_label(st.brand.eq_ignore_ascii_case(b), *b);
                    register(ui, format!("button:cameraProfileBrand-{b}"), r.rect);
                    if r.clicked() {
                        st.brand = (*b).to_string();
                        st.model.clear();
                    }
                }
            });
            crate::widgets::name_combo(&r.response, &crate::i18n::tr_format!("Brand: {brand}", brand = shown));
            register(ui, "combo:cameraProfileBrand", r.response.rect);
            let all_models = crate::i18n::tr("All models").to_string();
            let shown = if st.model.is_empty() { all_models.clone() } else { st.model.clone() };
            ui.add_enabled_ui(!st.brand.is_empty(), |ui| {
                let r = crate::widgets::combo_box("settings:cameraProfileModel").width(170.0).selected_text(shown.clone()).show_ui(ui, |ui| {
                    let r = ui.selectable_label(st.model.is_empty(), &all_models);
                    register(ui, "button:cameraProfileModel-all", r.rect);
                    if r.clicked() {
                        st.model.clear();
                    }
                    for m in &models {
                        let r = ui.selectable_label(st.model.eq_ignore_ascii_case(m), *m);
                        register(ui, format!("button:cameraProfileModel-{m}"), r.rect);
                        if r.clicked() {
                            st.model = (*m).to_string();
                        }
                    }
                });
                crate::widgets::name_combo(&r.response, &crate::i18n::tr_format!("Model: {model}", model = shown));
                register(ui, "combo:cameraProfileModel", r.response.rect);
            });
            field(
                ui,
                crate::text_field::TextField::singleline("field:cameraProfileFilter", &mut st.name)
                    .hint(crate::i18n::tr("Profile name"))
                    .width(150.0),
            );
        });
        let needle = st.name.trim().to_lowercase();
        let shown: Vec<&GridRow> = rows
            .iter()
            .filter(|r| st.brand.is_empty() || r.brand.eq_ignore_ascii_case(&st.brand))
            .filter(|r| st.model.is_empty() || r.model.eq_ignore_ascii_case(&st.model))
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
            .collect();
        ui.horizontal(|ui| {
            let r = ui.label(
                RichText::new(crate::i18n::tr_format!(
                    "{n} of {m} camera profiles · {k} selected",
                    n = shown.len(),
                    m = rows.len(),
                    k = st.selected.len()
                ))
                .size(11.5)
                .color(t.text_dim),
            );
            register(ui, "settings:cameraProfileCount", r.rect);
            let label = crate::i18n::tr_format!("Delete Selected ({n})", n = st.selected.len());
            let b = ui.add_enabled(!st.selected.is_empty(), egui::Button::new(label));
            register(ui, "button:deleteSelectedCameraProfiles", b.rect);
            if b.clicked() {
                let mut ids: Vec<String> = st.selected.iter().cloned().collect();
                ids.sort();
                delete = Some(ids);
            }
        });
        let height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let all_on = !shown.is_empty() && shown.iter().all(|r| st.selected.contains(&r.id));
        egui_extras::TableBuilder::new(ui)
            .id_salt("settings:cameraProfileGrid")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .max_scroll_height(LIST_HEIGHT)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::exact(24.0))
            .column(egui_extras::Column::initial(100.0).at_least(60.0).clip(true))
            .column(egui_extras::Column::initial(150.0).at_least(60.0).clip(true))
            .column(egui_extras::Column::initial(200.0).at_least(80.0).clip(true))
            .column(egui_extras::Column::remainder().at_least(70.0).clip(true))
            .header(height, |mut h| {
                h.col(|ui| {
                    let mut on = all_on;
                    // ticks (or unticks) every profile the filters show
                    if named_check(ui, "check:cameraProfileSelectAll".into(), &mut on, crate::i18n::tr("Select all shown").to_string()) {
                        for r in &shown {
                            if on {
                                st.selected.insert(r.id.clone());
                            } else {
                                st.selected.remove(&r.id);
                            }
                        }
                    }
                });
                for title in ["Brand", "Model", "Profile", "Source"] {
                    h.col(|ui| {
                        ui.label(RichText::new(crate::i18n::tr(title)).strong().color(t.text));
                    });
                }
            })
            .body(|body| {
                body.rows(height, shown.len(), |mut row| {
                    let Some(r) = shown.get(row.index()) else { return };
                    row.col(|ui| {
                        let mut on = st.selected.contains(&r.id);
                        let name = crate::i18n::tr_format!("Select {name} ({camera})", name = r.name, camera = r.camera);
                        if named_check(ui, format!("check:cameraProfile:{}", r.id), &mut on, name) {
                            if on {
                                st.selected.insert(r.id.clone());
                            } else {
                                st.selected.remove(&r.id);
                            }
                        }
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&r.brand).color(t.text_label));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&r.model).color(t.text_label));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&r.name).color(t.text));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(&r.source).color(t.text_dim)).on_hover_text(&r.file);
                    });
                });
            });
    }
    if hidden > 0 {
        ui.horizontal(|ui| {
            let r = ui.label(
                RichText::new(crate::i18n::tr_format!("{n} camera profiles from the folders are hidden", n = hidden)).size(11.5).color(t.text_dim),
            );
            register(ui, "settings:hiddenCameraProfiles", r.rect);
            let b = ui.button(crate::i18n::tr("Show Them Again"));
            register(ui, "button:showHiddenCameraProfiles", b.rect);
            show_hidden = b.clicked();
        });
    }
    ui.data_mut(|d| d.insert_temp(grid_state_id(), st));
    if show_hidden {
        run_or_toast(app, ui, "profiles.showHiddenCameraProfiles", json!({}));
    }
    if let Some(ids) = delete {
        match app.run("profile.deleteCamera", json!({"ids": ids})) {
            Ok(r) => {
                let gone = r["deleted"].as_array().map_or(0, Vec::len);
                let failed = r["failed"].as_array().map_or(0, Vec::len);
                if failed > 0 {
                    app.toast_error(ui.ctx(), crate::i18n::tr_format!("{n} camera profiles deleted · {m} could not be", n = gone, m = failed));
                } else {
                    app.toast(ui.ctx(), crate::i18n::tr_format!("{n} camera profiles deleted", n = gone));
                }
            }
            Err(e) => app.toast_error(ui.ctx(), e),
        }
    }
}

fn folders_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "Camera profile folders (experimental)");
    hint(
        ui,
        t,
        "Camera profiles (.dcp) in these folders are used where they are: nothing is copied. Put profiles in your own folder, or add other folders. Changing the folders reads every camera profile again.",
    );
    let report = match app.run("profiles.cameraFolders", json!({})) {
        Ok(r) => r,
        Err(e) => {
            ui.label(RichText::new(e).color(t.caution));
            return;
        }
    };
    let automatic = report["automatic"].as_bool().unwrap_or(true);
    row(ui, t, "Automatic", |ui| {
        let mut on = automatic;
        if check(ui, "automaticCameraProfile", &mut on, "Give a raw with no camera profile one made for its camera") {
            run_or_toast(app, ui, "profiles.setAutomaticCameraProfile", json!({"on": on}));
        }
    });
    // the default profiles' names: saved when the edit ends (Return or leaving the field)
    let saved = report["defaultName"].as_str().unwrap_or_default().to_string();
    let name_id = egui::Id::unique("settings:defaultCameraProfileName");
    let mut name: String = ui.data_mut(|d| d.get_temp::<String>(name_id)).unwrap_or_else(|| saved.clone());
    row(ui, t, "Default profiles", |ui| {
        let r = field(
            ui,
            crate::text_field::TextField::singleline("field:defaultCameraProfile", &mut name)
                .hint(crate::i18n::tr("Profile names, separated by commas"))
                .width(300.0),
        );
        if r.committed() && name.trim() != saved {
            run_or_toast(app, ui, "profiles.setDefaultCameraProfile", json!({"name": name.trim()}));
        }
        if r.editing {
            ui.data_mut(|d| d.insert_temp(name_id, name.clone()));
        } else {
            ui.data_mut(|d| d.remove::<String>(name_id));
        }
    });
    hint(
        ui,
        t,
        "A raw with no camera profile chosen develops with a profile made for its camera model: its only one or, when it has several, the first of these names it has, else the one whose name the most cameras have. A name picks a profile named so, or one holding it as whole words. A DNG keeps its own colour profile.",
    );
    let scanning = report["scanning"].as_bool().unwrap_or(false);
    row(ui, t, "Found", |ui| {
        let text = if scanning {
            crate::i18n::tr("Reading…").to_string()
        } else {
            crate::i18n::tr_format!(
                "{n} profiles for {c} cameras",
                n = report["profiles"].as_u64().unwrap_or(0),
                c = report["cameras"].as_u64().unwrap_or(0)
            )
        };
        let r = ui.label(text);
        register(ui, "settings:folderProfileCount", r.rect);
        let failed = report["failedCount"].as_u64().unwrap_or(0);
        if failed > 0 {
            let r = ui.label(RichText::new(crate::i18n::tr_format!("{n} unreadable", n = failed)).color(t.caution));
            register(ui, "settings:folderProfileFailed", r.rect);
            let tip: Vec<String> = report["failed"]
                .as_array()
                .map(|a| a.iter().take(10).filter_map(|f| Some(format!("{}: {}", f["path"].as_str()?, f["error"].as_str()?))).collect())
                .unwrap_or_default();
            r.on_hover_text(tip.join("\n"));
        }
        let b = ui.add_enabled(!scanning, egui::Button::new(crate::i18n::tr("Reload All")));
        register(ui, "button:reloadCameraProfiles", b.rect);
        if b.clicked() {
            run_or_toast(app, ui, "profiles.rescanCameraFolders", json!({}));
        }
    });
    let font = egui::TextStyle::Body.resolve(ui.style());
    let folders: Vec<String> =
        report["folders"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    if let Some(roots) = report["roots"].as_array() {
        for (i, r) in roots.iter().enumerate() {
            let path = r["path"].as_str().unwrap_or_default().to_string();
            let chosen = folders.contains(&path);
            let own = i == 0 && !chosen && path.ends_with(lightcraft_engine::dcp_folders::USER_FOLDER);
            row(ui, t, if i == 0 { "Folders" } else { "" }, |ui| {
                let found = if r["found"].as_bool() == Some(true) {
                    crate::i18n::tr_format!("{n} profiles", n = r["profiles"].as_u64().unwrap_or(0))
                } else {
                    crate::i18n::tr("not found").to_string()
                };
                let status = if own { crate::i18n::tr_format!("{found} · your folder", found = found) } else { found };
                let after = super::widgets::label_width(ui, &status, egui::TextStyle::Small)
                    + if chosen { super::widgets::button_width(ui, crate::i18n::tr("Remove")) } else { 0.0 };
                path_label(ui, &format!("profileFolder:{i}"), &path, font.clone(), t.text_label, after);
                ui.label(RichText::new(status).size(11.0).color(t.text_dim));
                if chosen {
                    let b = ui.small_button(crate::i18n::tr("Remove"));
                    register(ui, format!("button:removeProfileFolder:{i}"), b.rect);
                    crate::widgets::announce(&b, crate::i18n::tr_format!("Remove {path}", path = path));
                    if b.clicked() {
                        let kept: Vec<&String> = folders.iter().filter(|f| **f != path).collect();
                        run_or_toast(app, ui, "profiles.setCameraFolders", json!({"folders": kept}));
                    }
                }
            });
        }
    }
    row(ui, t, "", |ui| {
        let b = ui.button(crate::i18n::tr("Add Folder…"));
        register(ui, "button:addProfileFolder", b.rect);
        if b.clicked() {
            run_or_toast(app, ui, "file.addCameraProfileFolder", json!({}));
        }
    });
    // the active raw photo: which profile it develops with
    if let Some(p) = app.session.active().and_then(|id| app.session.catalog.photo(id)).filter(|p| p.develops_raw()).map(|p| (**p).clone()) {
        // how many profiles the folders have for its camera (hidden ones left out)
        let found = app.session.media.dcp_folders.snapshot.for_camera(&p.meta.camera, "").len();
        let camera = p.meta.camera.clone();
        let text = match app.session.media.auto_camera_profile(&p) {
            Some(e) => crate::i18n::tr_format!("{camera}: {name} (automatic)", camera = camera, name = e.name),
            None if !automatic => crate::i18n::tr_format!("{camera}: the camera's own colour (automatic choice off)", camera = camera),
            None if p.format.eq_ignore_ascii_case("DNG") => crate::i18n::tr_format!("{camera}: the DNG's own colour profile", camera = camera),
            None if found > 0 => crate::i18n::tr_format!(
                "{camera}: the camera's own colour (none of its {n} camera profiles has one of these names, or a name more cameras have than the others)",
                camera = camera,
                n = found
            ),
            None => crate::i18n::tr_format!("{camera}: the camera's own colour (no camera profile for this model in the folders)", camera = camera),
        };
        row(ui, t, "Selected photo", |ui| {
            let r = ui.label(text);
            register(ui, "settings:folderProfileForActive", r.rect);
        });
    }
}
