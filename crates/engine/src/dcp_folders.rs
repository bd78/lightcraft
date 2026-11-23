//! Camera profiles (`.dcp`, the DNG specification's camera profile files, read by [`lightcraft_raw::dcp`]) in folders
//! the user names: their own per-user folder ([`user_folder`]), the folders added in Settings ▸ Profiles and those in
//! `LIGHTCRAFT_DCP_DIRS`. They are used where they lie, at run time, like an imported camera profile
//! ([`crate::cmd::dcp_profiles`]): nothing is copied into a library, and nothing is shipped.
//!
//! EXPERIMENTAL (branch only, not for `main` or a release).
//!
//! The folders are walked on a worker thread ([`DcpFolders::rescan`]); what each file holds (its `ProfileName` and
//! `UniqueCameraModel`, a fingerprint of its bytes) is kept in an index, saved in the config folder so the next start
//! re-reads only the files that changed. A change of the folders reads every file again. A profile itself is parsed
//! only when a photo develops with it, and only a few are kept parsed ([`LOADED`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::activity::{Activity, Cancel};
use crate::media::LoadedCameraProfile;

/// Profile id prefix of a camera profile from a folder (the imported ones' `dcp:`, so `develop.profile`, the decoder
/// and the render keys treat both alike).
pub const ID_PREFIX: &str = "dcp:dir-";
/// The profile browser's group of camera profiles from the folders.
pub const GROUP: &str = "Camera Profile Folders";
/// The per-user folder of camera profiles, in the config folder ([`user_folder`]).
pub const USER_FOLDER: &str = "Camera Profiles";
/// Settings file in the config folder: the added folders and the default profiles' names.
pub const SETTINGS_FILE: &str = "camera-profile-folders.json";
/// The index of the last scan, in the config folder.
pub const INDEX_FILE: &str = "camera-profile-folders-index.json";
/// Parsed profiles kept in memory (one or two per camera in view).
const LOADED: usize = 16;
/// Failures kept for the report (a folder of damaged files must not grow the index without bound).
const MAX_FAILURES: usize = 200;
/// Index format version: an index of another version is read again from the files.
const INDEX_VERSION: u32 = 1;
/// The activity stack's label for a scan (English; the stack translates it).
pub const SCAN_LABEL: &str = "Reading camera profile folders";

/// What the user chose ([`SETTINGS_FILE`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Folders the user added (searched after the per-user one).
    #[serde(default)]
    pub folders: Vec<String>,
    /// A raw with no camera profile chosen develops with a profile made for its camera ([`Snapshot::default_for`]).
    #[serde(default = "yes")]
    pub automatic: bool,
    /// Of a camera's several profiles, the one a raw develops with: the first of these names (separated by commas,
    /// [`default_names`]) it has, else the one whose name the most cameras have ([`Snapshot::default_for`]). A camera
    /// with one profile develops with it whatever its name.
    #[serde(default)]
    pub default_name: String,
    /// Profiles taken out of LightCraft (their ids, [`Entry::id`]): not listed and never chosen automatically; their
    /// files stay where they are. A photo developed with one keeps it.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// One profile file in a folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch (0 when the file system has none): with the size, what
    /// tells a changed file.
    pub modified: u64,
    /// [`LoadedCameraProfile::fingerprint`] of the file's bytes.
    pub fingerprint: u64,
    /// `ProfileName`, else the file name.
    pub name: String,
    /// `UniqueCameraModel`.
    pub camera_model: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { folders: Vec::new(), automatic: true, default_name: String::new(), hidden: Vec::new() }
    }
}

fn yes() -> bool {
    true
}

impl Entry {
    /// The profile id photos are developed with.
    pub fn id(&self) -> String {
        format!("{ID_PREFIX}{:016x}", self.fingerprint)
    }
}

/// A file that could not be read as a camera profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub path: String,
    pub error: String,
}

/// One folder searched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub path: String,
    /// The folder is there.
    pub found: bool,
    /// `.dcp` files found in it.
    pub files: usize,
    /// Of which read as camera profiles.
    pub profiles: usize,
    /// The walk stopped at its limits (there may be more files).
    pub truncated: bool,
}

/// What a scan found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    #[serde(default)]
    pub version: u32,
    pub roots: Vec<Root>,
    pub entries: Vec<Entry>,
    /// The first [`MAX_FAILURES`] failures.
    pub failed: Vec<Failure>,
    /// All failures.
    #[serde(default)]
    pub failed_count: usize,
}

/// The index with its camera lookup, as photos are developed with it (replaced whole by a new scan).
#[derive(Debug, Default)]
pub struct Snapshot {
    pub index: Index,
    /// [`camera_key`] → entries (indices into `index.entries`), the hidden ones left out.
    by_camera: HashMap<String, Vec<usize>>,
    /// A profile name ([`name_words`] joined) → how many cameras have a profile of that name, the hidden ones left
    /// out ([`Self::most_shared`]).
    name_cameras: HashMap<String, usize>,
    /// Fingerprints of the profiles hidden ([`Settings::hidden`]).
    hidden: std::collections::HashSet<u64>,
}

impl Snapshot {
    pub fn new(index: Index) -> Snapshot {
        Snapshot::hiding(index, &[])
    }

    /// The index with the profiles of ids `hidden` left out of every list (but [`Self::by_id`]).
    pub fn hiding(index: Index, hidden: &[String]) -> Snapshot {
        let hidden: std::collections::HashSet<u64> =
            hidden.iter().filter_map(|id| id.strip_prefix(ID_PREFIX)).filter_map(|h| u64::from_str_radix(h, 16).ok()).collect();
        let mut by_camera: HashMap<String, Vec<usize>> = HashMap::new();
        // each profile name, with the cameras that have a profile of that name
        let mut named: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for (i, e) in index.entries.iter().enumerate() {
            let key = camera_key(&e.camera_model);
            if !key.is_empty() && !hidden.contains(&e.fingerprint) {
                named.entry(name_words(&e.name).join(" ")).or_default().insert(key.clone());
                by_camera.entry(key).or_default().push(i);
            }
        }
        let name_cameras = named.into_iter().map(|(name, cameras)| (name, cameras.len())).collect();
        Snapshot { index, by_camera, name_cameras, hidden }
    }

    /// Is `e` hidden ([`Settings::hidden`])?
    pub fn is_hidden(&self, e: &Entry) -> bool {
        self.hidden.contains(&e.fingerprint)
    }

    /// The profiles found, the hidden ones left out.
    pub fn shown(&self) -> impl Iterator<Item = &Entry> {
        self.index.entries.iter().filter(|e| !self.is_hidden(e))
    }

    /// How many of the profiles found are hidden.
    pub fn hidden_count(&self) -> usize {
        self.index.entries.iter().filter(|e| self.is_hidden(e)).count()
    }

    /// The folder searched that `e` was found in (the deepest one holding it).
    pub fn root_of(&self, e: &Entry) -> Option<&Root> {
        self.index.roots.iter().filter(|r| Path::new(&e.path).starts_with(&r.path)).max_by_key(|r| r.path.len())
    }

    /// The entry with profile id `id` (hidden or not: a photo developed with a hidden profile keeps it).
    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        let hex = id.strip_prefix(ID_PREFIX)?;
        let fingerprint = u64::from_str_radix(hex, 16).ok()?;
        self.index.entries.iter().find(|e| e.fingerprint == fingerprint)
    }

    /// The profiles made for `camera` (the catalog's camera: Exif make and model joined), in no order. Tried as the
    /// whole name, then without its leading words one by one (the make, or a make written twice: "NIKON CORPORATION
    /// NIKON D850", "RICOH IMAGING COMPANY, LTD. PENTAX K-1").
    fn of_camera(&self, camera: &str) -> Vec<&Entry> {
        camera_candidates(camera)
            .iter()
            .find_map(|k| self.by_camera.get(k))
            .map(|ix| ix.iter().filter_map(|&i| self.index.entries.get(i)).collect())
            .unwrap_or_default()
    }

    /// The profiles made for `camera`, its default one ([`Self::default_for`] with `names`) first, then by name.
    pub fn for_camera(&self, camera: &str, names: &str) -> Vec<&Entry> {
        let first = self.default_for(camera, names).map(|e| e.fingerprint);
        let mut out = self.of_camera(camera);
        out.sort_by_cached_key(|e| (Some(e.fingerprint) != first, e.name.to_lowercase()));
        out
    }

    /// The profile a raw of `camera` develops with when none is chosen: the camera's only one; of several, the first
    /// of `names` ([`default_names`]) the camera has (a profile named so exactly comes before one holding the name as
    /// whole words, "Muted" in "Soft Muted"; then the newest file), else the one whose name the most cameras have
    /// ([`Self::most_shared`]). None when the camera has no profile, or several and none of them is picked so.
    pub fn default_for(&self, camera: &str, names: &str) -> Option<&Entry> {
        let all = self.of_camera(camera);
        if let [only] = all.as_slice() {
            return Some(*only);
        }
        default_names(names)
            .into_iter()
            .find_map(|name| all.iter().copied().filter(|e| name_match(&e.name, name) > 0).max_by_key(|e| (name_match(&e.name, name), e.modified)))
            .or_else(|| self.most_shared(&all))
    }

    /// Of one camera's `profiles`, the one whose name the most cameras have a profile of (the folders' everyday
    /// profile, not a look made for a few cameras), the newest file of that name. None when no other camera has a
    /// profile of any of these names, or when two names are as common.
    fn most_shared<'a>(&self, profiles: &[&'a Entry]) -> Option<&'a Entry> {
        let name = |e: &Entry| name_words(&e.name).join(" ");
        let cameras = |e: &Entry| self.name_cameras.get(&name(e)).copied().unwrap_or(0);
        let most = profiles.iter().map(|e| cameras(e)).max().filter(|&n| n > 1)?;
        let best: Vec<&'a Entry> = profiles.iter().copied().filter(|e| cameras(e) == most).collect();
        let first = name(best.first()?);
        if best.iter().any(|e| name(e) != first) {
            return None;
        }
        best.into_iter().max_by_key(|e| e.modified)
    }

    /// How many cameras have a profile.
    pub fn cameras(&self) -> usize {
        self.by_camera.len()
    }
}

/// The names of a list of default profiles ([`Settings::default_name`]), in the order they are tried: separated by
/// commas or semicolons, spaces around them ignored.
pub fn default_names(list: &str) -> Vec<&str> {
    list.split([',', ';']).map(str::trim).filter(|n| !n.is_empty()).collect()
}

/// How a profile named `profile` answers to the name `wanted` (case and punctuation ignored): 2 named so, 1 holding
/// it as whole words ("Muted" in "Soft Muted", not "Mute"), 0 not at all.
fn name_match(profile: &str, wanted: &str) -> u8 {
    let (p, w) = (name_words(profile), name_words(wanted));
    if w.is_empty() {
        0
    } else if p == w {
        2
    } else if p.windows(w.len()).any(|x| x == w.as_slice()) {
        1
    } else {
        0
    }
}

/// The words of a profile's name as names are compared: lower case, split at punctuation and spaces.
fn name_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// A camera's name for display: its maker and model ([`camera_brand`]), the maker not written twice ("Sony
/// ILCE-7M4", "Apple iPhone 15 Pro", "Nikon Z 8").
pub fn camera_title(camera: &str) -> String {
    let (brand, model) = camera_brand(camera);
    if brand.is_empty() || model.to_lowercase().starts_with(&brand.to_lowercase()) { model } else { format!("{brand} {model}") }
}

/// `s` folded for comparing camera names: lower case, with no punctuation or spacing (a camera writes "E-M5MarkII"
/// where a profile says "E-M5 Mark II", "Z 8" for "Z8"), "Mark" and its numeral as Canon's Exif writes them ("R6 Mark
/// II" is "R6m2"), make suffixes ("corporation", "imaging", "company"…) dropped and a word repeated next to itself
/// once ("canon canon eos r5").
pub fn camera_key(s: &str) -> String {
    fold_marks(&camera_words(s).concat())
}

/// The words of `s` as [`camera_key`] compares them: lower case, split at punctuation and spaces, make suffixes and
/// a word repeated next to itself left out.
fn camera_words(s: &str) -> Vec<String> {
    const NOISE: &[&str] = &["corporation", "corp", "co", "ltd", "inc", "imaging", "company", "optical", "electric", "industrial"];
    let folded: String = s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect();
    let mut words: Vec<&str> = Vec::new();
    for w in folded.split_whitespace() {
        if NOISE.contains(&w) || words.last() == Some(&w) {
            continue;
        }
        words.push(w);
    }
    words.into_iter().map(str::to_string).collect()
}

/// `key` with each "mark" and the Roman numeral after it as "m" and the number ("markii" → "m2").
fn fold_marks(key: &str) -> String {
    const NUMERALS: &[&str] = &["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"];
    let mut parts = key.split("mark");
    let mut out = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let rest = part.trim_start_matches(['i', 'v', 'x']);
        match part.strip_suffix(rest).and_then(|numeral| NUMERALS.iter().position(|n| *n == numeral)) {
            Some(i) => out.push_str(&format!("m{}{rest}", i + 1)),
            None => out.push_str(&format!("mark{part}")),
        }
    }
    out
}

/// Is a profile made for `profile_camera` (its `UniqueCameraModel`) one for photos of `catalog_camera` (Exif make
/// and model joined)?
pub fn same_camera(catalog_camera: &str, profile_camera: &str) -> bool {
    let k = camera_key(profile_camera);
    !k.is_empty() && camera_candidates(catalog_camera).contains(&k)
}

/// The maker a profile's camera name starts with, and the rest of the name: ("Sony", "ILCE-7M4") for "SONY ILCE-7M4".
/// Most makers are one word; the few written in more are listed. A name of capitals or of small letters only is
/// written with a capital first ("SONY", "samsung"), a short or mixed one as it is ("DJI", "GoPro"). Empty for a
/// name with no words.
pub fn camera_brand(camera: &str) -> (String, String) {
    const MANY_WORDS: &[&[&str]] = &[&["om", "digital", "solutions"], &["phase", "one"], &["konica", "minolta"], &["sony", "ericsson"]];
    // phones and tablets are often named by their product line alone ("iPhone 15 Pro", "Pixel 8"): its maker
    const LINES: &[(&str, &str)] = &[
        ("iphone", "Apple"),
        ("ipad", "Apple"),
        ("ipod", "Apple"),
        ("pixel", "Google"),
        ("nexus", "Google"),
        ("galaxy", "Samsung"),
        ("redmi", "Xiaomi"),
        ("poco", "Xiaomi"),
        ("xperia", "Sony"),
        ("lumia", "Nokia"),
        ("moto", "Motorola"),
    ];
    let words: Vec<&str> = camera.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    if let Some(first) = lower.first()
        && let Some((_, maker)) = LINES.iter().find(|(line, _)| first.starts_with(line))
    {
        return ((*maker).to_string(), camera.trim().to_string());
    }
    let n = MANY_WORDS.iter().find(|m| lower.len() > m.len() && m.iter().zip(&lower).all(|(a, b)| a == b)).map_or(1, |m| m.len()).min(words.len());
    let brand: Vec<String> = words.iter().take(n).map(|w| written(w)).collect();
    let rest = words.get(n..).unwrap_or_default().join(" ");
    (brand.join(" "), if rest.is_empty() { camera.trim().to_string() } else { rest })
}

/// A maker's word as a menu shows it (see [`camera_brand`]).
fn written(w: &str) -> String {
    let letters = w.chars().filter(|c| c.is_alphabetic());
    let same_case = letters.clone().all(char::is_uppercase) || letters.clone().all(char::is_lowercase);
    if !same_case || w.chars().count() <= 3 && w.chars().any(char::is_uppercase) {
        return w.to_string();
    }
    let mut c = w.chars();
    c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
}

/// [`camera_key`] of `camera`, then of it without its first word, its first two…, down to two words (one word alone
/// is too little to tell cameras apart).
fn camera_candidates(camera: &str) -> Vec<String> {
    let words = camera_words(camera);
    let mut out = Vec::new();
    for skip in 0..words.len() {
        if words.len() - skip < 2 && skip > 0 {
            break;
        }
        out.push(fold_marks(&words.get(skip..).unwrap_or_default().concat()));
    }
    out
}

/// The user's own folder of camera profiles in the config folder `config` ([`USER_FOLDER`]), made when it isn't
/// there yet so profiles can be put in it.
pub fn user_folder(config: &Path) -> PathBuf {
    let d = config.join(USER_FOLDER);
    if let Err(e) = std::fs::create_dir_all(&d) {
        log::info!("camera profile folders: can't make {}: {e}", d.display());
    }
    d
}

/// Folders named in `LIGHTCRAFT_DCP_DIRS` (the platform's path-list separator).
pub fn env_roots() -> Vec<PathBuf> {
    std::env::var_os("LIGHTCRAFT_DCP_DIRS").map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect()).unwrap_or_default()
}

fn modified_ns(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Walk `roots` for camera profiles. A file with the path, size and modification time it had in `previous` is not
/// read again. A profile found under two folders (the same bytes) is listed once, under the first.
pub fn scan(roots: &[PathBuf], previous: &Index) -> Index {
    let known: HashMap<&str, &Entry> = previous.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut index = Index { version: INDEX_VERSION, ..Index::default() };
    let mut seen = std::collections::HashSet::new();
    let mut seen_roots = std::collections::HashSet::new();
    for root in roots {
        if !seen_roots.insert(root.clone()) {
            continue;
        }
        let shown = root.display().to_string();
        if !root.is_dir() {
            index.roots.push(Root { path: shown, found: false, files: 0, profiles: 0, truncated: false });
            continue;
        }
        let walked =
            crate::walk::files_in(root, None, crate::walk::Limits::default(), |f| f.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp")));
        let mut r = Root { path: shown, found: true, files: walked.files.len(), profiles: 0, truncated: walked.truncated };
        for f in walked.files {
            let path = f.display().to_string();
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            let (size, modified) = (meta.len(), modified_ns(&meta));
            let entry = match known.get(path.as_str()) {
                Some(e) if e.size == size && e.modified == modified => Ok((*e).clone()),
                _ => read_entry(&f, path.clone(), size, modified),
            };
            match entry {
                Ok(e) => {
                    r.profiles += 1;
                    if seen.insert(e.fingerprint) {
                        index.entries.push(e);
                    }
                }
                Err(error) => {
                    index.failed_count += 1;
                    if index.failed.len() < MAX_FAILURES {
                        index.failed.push(Failure { path, error });
                    }
                }
            }
        }
        index.roots.push(r);
    }
    index
}

fn read_entry(f: &Path, path: String, size: u64, modified: u64) -> Result<Entry, String> {
    let bytes = lightcraft_raw::dcp::read_dcp(f).map_err(|e| e.to_string())?;
    let loaded = LoadedCameraProfile::parse(&bytes).map_err(|e| e.to_string())?;
    let p = &loaded.profile;
    let name = if p.name.trim().is_empty() {
        f.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Camera Profile".into())
    } else {
        p.name.trim().to_string()
    };
    Ok(Entry { path, size, modified, fingerprint: loaded.fingerprint, name, camera_model: p.camera_model.trim().to_string() })
}

/// The camera profile folders of a session: the folders, the user's choices, the current index and the profiles
/// parsed so far. `Default` finds nothing (tests, the web): [`DcpFolders::new`] with folders, then
/// [`DcpFolders::rescan`], make it look.
#[derive(Default)]
pub struct DcpFolders {
    /// The folders searched before the added ones ([`user_folder`], [`env_roots`]).
    base_roots: Vec<PathBuf>,
    pub settings: Settings,
    /// Where [`Settings`] and the index are saved (`None`: not saved).
    settings_file: Option<PathBuf>,
    index_file: Option<PathBuf>,
    pub snapshot: Arc<Snapshot>,
    /// A scan under way: its index lands here.
    pending: Option<Mutex<Receiver<Index>>>,
    loaded: Mutex<Vec<LoadedCameraProfile>>,
}

impl DcpFolders {
    /// Profiles under `base_roots` and the user's folders, with the choices and the last index read from
    /// `config` (when there is one). Nothing is scanned yet.
    pub fn new(base_roots: Vec<PathBuf>, config: Option<&Path>) -> DcpFolders {
        let settings_file = config.map(|d| d.join(SETTINGS_FILE));
        let index_file = config.map(|d| d.join(INDEX_FILE));
        let settings = settings_file.as_deref().and_then(read_json::<Settings>).unwrap_or_default();
        let index = index_file.as_deref().and_then(read_json::<Index>).filter(|i| i.version == INDEX_VERSION).unwrap_or_default();
        let snapshot = Arc::new(Snapshot::hiding(index, &settings.hidden));
        DcpFolders { base_roots, settings, settings_file, index_file, snapshot, ..DcpFolders::default() }
    }

    /// Every folder searched, the per-user one first.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut out = self.base_roots.clone();
        out.extend(self.settings.folders.iter().map(PathBuf::from));
        out
    }

    /// Is a scan under way?
    pub fn scanning(&self) -> bool {
        self.pending.is_some()
    }

    /// Walk the folders again on a worker thread (shown in the activity stack); [`Self::poll`] takes its index. Where
    /// no thread can be started the walk runs here. `fresh` reads every file again; otherwise a file that has not
    /// changed since the last scan is not read.
    pub fn rescan(&mut self, activity: &Activity, fresh: bool) {
        let previous = if fresh { Index::default() } else { self.snapshot.index.clone() };
        let (roots, index_file) = (self.roots(), self.index_file.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let activity = activity.clone();
        let job = move || {
            let _row = activity.start("profiles", SCAN_LABEL, Cancel::No);
            let index = scan(&roots, &previous);
            if let Some(f) = &index_file {
                write_json(f, &index);
            }
            let _ = tx.send(index);
        };
        match std::thread::Builder::new().name("dcp-folders-scan".into()).spawn(job) {
            Ok(_) => self.pending = Some(Mutex::new(rx)),
            Err(e) => {
                log::info!("camera profile folders: can't start the scan thread ({e}); scanning here");
                self.rescan_now(fresh);
            }
        }
    }

    /// [`Self::rescan`] on this thread (the CLI, which develops right after).
    pub fn rescan_now(&mut self, fresh: bool) {
        let previous = if fresh { Index::default() } else { self.snapshot.index.clone() };
        let index = scan(&self.roots(), &previous);
        if let Some(f) = &self.index_file {
            write_json(f, &index);
        }
        self.pending = None;
        self.take(index);
    }

    /// Take the index of a finished scan. True when it changed the profiles found.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else { return false };
        let received = rx.lock().unwrap_or_else(PoisonError::into_inner).try_recv();
        let index = match received {
            Ok(index) => index,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                return false;
            }
        };
        self.pending = None;
        let changed = index.entries != self.snapshot.index.entries;
        self.take(index);
        changed
    }

    /// Wait for a scan under way to finish (a command asked to).
    pub fn wait(&mut self) {
        if let Some(rx) = self.pending.take()
            && let Ok(index) = rx.into_inner().unwrap_or_else(PoisonError::into_inner).recv()
        {
            self.take(index);
        }
    }

    fn take(&mut self, index: Index) {
        self.snapshot = Arc::new(Snapshot::hiding(index, &self.settings.hidden));
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// Hide (`hide`) or show again the profiles of `ids`, and save the choice. Their files are never touched.
    pub fn set_hidden(&mut self, ids: &[String], hide: bool) -> Result<(), String> {
        let hidden = &mut self.settings.hidden;
        if hide {
            for id in ids {
                if !hidden.contains(id) {
                    hidden.push(id.clone());
                }
            }
        } else {
            hidden.retain(|h| !ids.contains(h));
        }
        let snapshot = Arc::new(Snapshot::hiding(self.snapshot.index.clone(), &self.settings.hidden));
        self.snapshot = snapshot;
        self.save_settings()
    }

    /// Save the choices (an error says why they could not be).
    pub fn save_settings(&self) -> Result<(), String> {
        let Some(f) = &self.settings_file else { return Ok(()) };
        let json = serde_json::to_vec_pretty(&self.settings).map_err(|e| e.to_string())?;
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        std::fs::write(f, json).map_err(|e| format!("{}: {e}", f.display()))
    }

    /// The parsed profile of `entry` (read from its file the first time; `None` when it can no longer be read, or
    /// holds other bytes than when it was indexed).
    pub fn load(&self, entry: &Entry) -> Option<LoadedCameraProfile> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = loaded.iter().position(|l| l.fingerprint == entry.fingerprint) {
            let l = loaded.remove(i);
            loaded.push(l.clone());
            return Some(l);
        }
        let bytes = lightcraft_raw::dcp::read_dcp(Path::new(&entry.path)).ok()?;
        let l = LoadedCameraProfile::parse(&bytes).ok().filter(|l| l.fingerprint == entry.fingerprint)?;
        if loaded.len() >= LOADED {
            loaded.remove(0);
        }
        loaded.push(l.clone());
        Some(l)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(f: &Path) -> Option<T> {
    let bytes = std::fs::read(f).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("{}: {e}", f.display());
            None
        }
    }
}

fn write_json<T: Serialize>(f: &Path, v: &T) {
    let written = serde_json::to_vec(v).map_err(|e| e.to_string()).and_then(|json| {
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(f, json).map_err(|e| e.to_string())
    });
    if let Err(e) = written {
        log::warn!("camera profile folders: could not save {}: {e}", f.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_names_fold_makes_written_twice_and_company_suffixes() {
        assert_eq!(camera_key("NIKON CORPORATION NIKON Z 8"), camera_key("Nikon Z 8"));
        assert_eq!(camera_key("Canon Canon EOS R5"), camera_key("Canon EOS R5"));
        assert_eq!(camera_key("SONY ILCE-7M4"), camera_key("Sony ILCE-7M4"));
        assert_ne!(camera_key("SONY ILCE-7M4"), camera_key("Sony ILCE-7M3"));
        // the make is dropped only as a whole leading word, never inside the model
        assert!(camera_candidates("RICOH IMAGING COMPANY, LTD. PENTAX K-1").contains(&camera_key("Pentax K-1")));
        assert!(!same_camera("Sony ILCE-7M4", "7M4"), "one word alone matches nothing");
    }

    #[test]
    fn a_cameras_name_matches_however_its_model_is_spaced_or_its_mark_written() {
        // Exif writes the model in one piece where a profile spaces it, or the other way round
        assert!(same_camera("OLYMPUS IMAGING CORP. E-M5MarkII", "Olympus E-M5 Mark II"));
        assert!(same_camera("OM Digital Solutions OM-1MarkII", "OM Digital Solutions OM-1 Mark II"));
        assert!(same_camera("NIKON CORPORATION NIKON Z8", "Nikon Z 8"));
        // "Mark" and its numeral, or Canon's short form of them
        assert!(same_camera("Canon Canon EOS R6m2", "Canon EOS R6 Mark II"));
        assert!(same_camera("Canon Canon EOS 5D Mark IV", "Canon EOS 5D MarkIV"));
        // but another mark, or another model, is another camera
        assert!(!same_camera("OLYMPUS IMAGING CORP. E-M5MarkII", "Olympus E-M5 Mark III"));
        assert!(!same_camera("OLYMPUS IMAGING CORP. E-M5MarkII", "Olympus E-M5"));
        assert!(!same_camera("Canon Canon EOS R6m2", "Canon EOS R6"));
    }

    #[test]
    fn a_cameras_title_names_its_maker_once() {
        assert_eq!(camera_title("Sony ILCE-7M4"), "Sony ILCE-7M4");
        assert_eq!(camera_title("iPhone 15 Pro"), "Apple iPhone 15 Pro", "a phone named by its line alone gets its maker");
        assert_eq!(camera_title("NIKON Z 8"), "Nikon Z 8");
        assert_eq!(camera_title("D850"), "D850", "one word is not written twice");
    }

    #[test]
    fn a_cameras_maker_is_its_first_word_or_a_maker_written_in_several() {
        // the same maker however it is written
        assert_eq!(camera_brand("SONY ILCE-7M4").0, camera_brand("Sony ILCE-7M3").0);
        assert_eq!(camera_brand("samsung SM-S911B").0, camera_brand("Samsung Galaxy S23").0);
        // a maker of several words keeps them all, and the model is what is left
        let (brand, model) = camera_brand("OM Digital Solutions OM-1");
        assert!(brand.split(' ').count() > 1 && !model.contains("Digital"), "{brand} / {model}");
        // a short or mixed-case maker is written as it is
        assert_eq!(camera_brand("GoPro HERO12").0, "GoPro");
        assert_eq!(camera_brand("DJI Mavic 3").0, "DJI");
        // a name of one word is its own model
        assert_eq!(camera_brand("Leaf").1, "Leaf");
        // a phone or tablet named by its product line goes under its maker, its whole name the model
        assert_eq!(camera_brand("iPhone 15 Pro back camera").0, camera_brand("Apple iPhone 15").0);
        assert_eq!(camera_brand("iPad Pro (12.9-inch)").0, camera_brand("Apple iPhone 15").0);
        assert_eq!(camera_brand("iPhone 15 Pro back camera").1, "iPhone 15 Pro back camera");
        assert_eq!(camera_brand("Pixel 8").0, camera_brand("Google Pixel 7").0);
    }
}
