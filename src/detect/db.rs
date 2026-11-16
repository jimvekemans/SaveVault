//! Save location database.
//!
//! SaveVault uses the community-maintained Ludusavi manifest (derived from PCGamingWiki,
//! MIT licensed) to know where tens of thousands of games keep their saves. The upstream
//! YAML is ~18 MB, so it is converted once into a compact gzip-compressed index that loads
//! in a fraction of a second, and refreshed with an HTTP conditional request.

use crate::providers::slugify_game_name;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Upstream location of the manifest.
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";
const USER_AGENT: &str = concat!("savevault/", env!("CARGO_PKG_VERSION"));
const MAX_MANIFEST_BYTES: u64 = 96 * 1024 * 1024;
const INDEX_FILE: &str = "save_locations.json.gz";
const META_FILE: &str = "save_locations.meta.json";
/// How long a cached copy is considered fresh before a conditional refresh is attempted.
pub const REFRESH_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// `when` constraint of a database path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DbWhen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
}

/// One path template of a game.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DbFile {
    pub path: String,
    #[serde(default)]
    pub save: bool,
    #[serde(default)]
    pub config: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<DbWhen>,
}

/// One game in the compact index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DbGame {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gog: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steam_extra: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gog_extra: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lutris: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub install_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub files: Vec<DbFile>,
}

/// Bookkeeping stored next to the index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DbMeta {
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub game_count: usize,
    #[serde(default)]
    pub source_url: String,
}

/// In-memory database with lookup indexes.
#[derive(Debug, Clone, Default)]
pub struct SaveDb {
    pub games: Vec<DbGame>,
    pub meta: DbMeta,
    by_steam: HashMap<u64, usize>,
    by_gog: HashMap<u64, usize>,
    by_name: HashMap<String, usize>,
    by_lutris: HashMap<String, usize>,
    by_install_dir: HashMap<String, Vec<usize>>,
}

/// Outcome of `update_database`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// Cached copy is recent; no request made.
    Fresh,
    /// Server confirmed the cached copy is current.
    NotModified,
    /// New copy downloaded and indexed.
    Updated { games: usize },
}

// ---- Upstream YAML shape -------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct RawId {
    #[serde(default)]
    id: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawIds {
    #[serde(default)]
    steam_extra: Vec<u64>,
    #[serde(default)]
    gog_extra: Vec<u64>,
    #[serde(default)]
    lutris: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawWhen {
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    store: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawFile {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    when: Vec<RawWhen>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawGame {
    #[serde(default)]
    files: BTreeMap<String, Option<RawFile>>,
    #[serde(default)]
    steam: Option<RawId>,
    #[serde(default)]
    gog: Option<RawId>,
    #[serde(default)]
    id: Option<RawIds>,
    #[serde(default)]
    install_dir: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    alias: Option<String>,
}

/// Normalized lookup key for a game title.
pub fn title_key(name: &str) -> String {
    let mut slug = slugify_game_name(name);
    for junk in ["_tm_", "_r_"] {
        slug = slug.replace(junk, "_");
    }
    slug
}

/// Title keys to try for `name`, most specific first (strips edition / remaster wording).
pub fn title_key_variants(name: &str) -> Vec<String> {
    const TAILS: &[&str] = &[
        "_ultimate_edition",
        "_complete_edition",
        "_deluxe_edition",
        "_gold_edition",
        "_special_edition",
        "_legendary_edition",
        "_anniversary_edition",
        "_game_of_the_year_edition",
        "_game_of_the_year",
        "_goty_edition",
        "_goty",
        "_definitive_edition",
        "_enhanced_edition",
        "_directors_cut",
        "_director_s_cut",
        "_remastered",
        "_remaster",
        "_hd",
        "_prologue",
        "_demo",
        "_alpha",
        "_beta",
    ];
    let base = title_key(name);
    let mut out = vec![base.clone()];
    let mut cur = base;
    loop {
        let mut stripped = false;
        for tail in TAILS {
            if let Some(s) = cur.strip_suffix(tail)
                && !s.is_empty()
            {
                cur = s.to_string();
                if !out.contains(&cur) {
                    out.push(cur.clone());
                }
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    // "the_witcher_3_wild_hunt_remastered" -> also try without a leading article.
    if let Some(s) = out[0].strip_prefix("the_") {
        let s = s.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

impl SaveDb {
    /// Builds the lookup indexes for `games`.
    pub fn from_games(games: Vec<DbGame>, meta: DbMeta) -> Self {
        let mut db = SaveDb {
            games,
            meta,
            ..Default::default()
        };
        for (idx, g) in db.games.iter().enumerate() {
            if let Some(id) = g.steam {
                db.by_steam.insert(id, idx);
            }
            if let Some(id) = g.gog {
                db.by_gog.insert(id, idx);
            }
            if let Some(slug) = &g.lutris {
                db.by_lutris.insert(slug.to_lowercase(), idx);
            }
            db.by_name.entry(title_key(&g.title)).or_insert(idx);
            for alias in &g.aliases {
                db.by_name.entry(title_key(alias)).or_insert(idx);
            }
            for dir in &g.install_dirs {
                db.by_install_dir
                    .entry(dir.to_lowercase())
                    .or_default()
                    .push(idx);
            }
        }
        // Secondary ids never override a primary id.
        for (idx, g) in db.games.iter().enumerate() {
            for id in &g.steam_extra {
                db.by_steam.entry(*id).or_insert(idx);
            }
            for id in &g.gog_extra {
                db.by_gog.entry(*id).or_insert(idx);
            }
        }
        db
    }

    /// Parses the upstream manifest YAML, keeping only games that list file locations.
    pub fn from_manifest_yaml(yaml: &[u8]) -> Result<Self> {
        let raw: BTreeMap<String, RawGame> =
            serde_norway::from_slice(yaml).context("Could not parse the save location manifest")?;

        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for (title, game) in &raw {
            if let Some(target) = &game.alias {
                aliases.entry(target.clone()).or_default().push(title.clone());
            }
        }

        let mut games = Vec::new();
        for (title, game) in raw {
            if game.files.is_empty() {
                continue;
            }
            let files: Vec<DbFile> = game
                .files
                .into_iter()
                .map(|(path, f)| {
                    let f = f.unwrap_or_default();
                    DbFile {
                        path,
                        save: f.tags.iter().any(|t| t == "save"),
                        config: f.tags.iter().any(|t| t == "config"),
                        when: f
                            .when
                            .into_iter()
                            .map(|w| DbWhen {
                                os: w.os,
                                store: w.store,
                            })
                            .collect(),
                    }
                })
                .collect();
            let ids = game.id.unwrap_or_default();
            games.push(DbGame {
                aliases: aliases.remove(&title).unwrap_or_default(),
                title,
                steam: game.steam.and_then(|s| s.id),
                gog: game.gog.and_then(|s| s.id),
                steam_extra: ids.steam_extra,
                gog_extra: ids.gog_extra,
                lutris: ids.lutris,
                install_dirs: game.install_dir.into_keys().collect(),
                files,
            });
        }
        if games.is_empty() {
            bail!("The save location manifest contained no games");
        }
        let meta = DbMeta {
            game_count: games.len(),
            source_url: MANIFEST_URL.to_string(),
            ..Default::default()
        };
        Ok(Self::from_games(games, meta))
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn by_steam_id(&self, id: u64) -> Option<&DbGame> {
        self.by_steam.get(&id).map(|&i| &self.games[i])
    }

    pub fn by_gog_id(&self, id: u64) -> Option<&DbGame> {
        self.by_gog.get(&id).map(|&i| &self.games[i])
    }

    pub fn by_lutris_slug(&self, slug: &str) -> Option<&DbGame> {
        self.by_lutris.get(&slug.to_lowercase()).map(|&i| &self.games[i])
    }

    /// Looks a game up by title, tolerating edition / remaster suffixes.
    pub fn by_title(&self, name: &str) -> Option<&DbGame> {
        title_key_variants(name)
            .iter()
            .find_map(|k| self.by_name.get(k))
            .map(|&i| &self.games[i])
    }

    /// Looks a game up by its install folder name, only when that name is unambiguous.
    pub fn by_install_dir_name(&self, dir_name: &str) -> Option<&DbGame> {
        match self.by_install_dir.get(&dir_name.to_lowercase()) {
            Some(list) if list.len() == 1 => Some(&self.games[list[0]]),
            _ => None,
        }
    }

    /// Writes the compact index + metadata into `dir`.
    pub fn save_to_dir(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        let tmp = dir.join(format!("{INDEX_FILE}.tmp"));
        {
            let file = File::create(&tmp)
                .with_context(|| format!("Failed to write {}", tmp.display()))?;
            let mut enc = GzEncoder::new(BufWriter::new(file), Compression::fast());
            serde_json::to_writer(&mut enc, &self.games)?;
            enc.finish()?.flush()?;
        }
        fs::rename(&tmp, dir.join(INDEX_FILE))?;
        fs::write(dir.join(META_FILE), serde_json::to_string_pretty(&self.meta)?)?;
        Ok(())
    }

    /// Loads the compact index from `dir`, if one was cached there.
    pub fn load_from_dir(dir: &Path) -> Result<Option<Self>> {
        let index = dir.join(INDEX_FILE);
        if !index.is_file() {
            return Ok(None);
        }
        let file = File::open(&index)
            .with_context(|| format!("Failed to open {}", index.display()))?;
        let games: Vec<DbGame> = serde_json::from_reader(BufReader::new(GzDecoder::new(file)))
            .with_context(|| format!("Corrupt save location index {}", index.display()))?;
        let meta = read_meta(dir);
        Ok(Some(Self::from_games(games, meta)))
    }
}

fn read_meta(dir: &Path) -> DbMeta {
    fs::read_to_string(dir.join(META_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Default cache folder for the database.
pub fn default_db_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("save_game_manager")
        .join("detection")
}

/// Downloads / refreshes the cached database in `dir`.
///
/// With `force == false` nothing is requested while the cache is younger than
/// [`REFRESH_AFTER`]; otherwise a conditional request is made so an unchanged manifest costs
/// a few hundred bytes.
pub fn update_database(dir: &Path, force: bool) -> Result<UpdateOutcome> {
    update_database_from(dir, MANIFEST_URL, force)
}

/// Same as [`update_database`] with an explicit source URL.
pub fn update_database_from(dir: &Path, url: &str, force: bool) -> Result<UpdateOutcome> {
    let mut meta = read_meta(dir);
    let have_index = dir.join(INDEX_FILE).is_file();

    if !force
        && have_index
        && let Some(checked) = meta.checked_at.or(meta.fetched_at)
        && Utc::now().signed_duration_since(checked).to_std().unwrap_or_default() < REFRESH_AFTER
    {
        return Ok(UpdateOutcome::Fresh);
    }

    let mut req = ureq::get(url).header("User-Agent", USER_AGENT);
    if have_index && let Some(etag) = &meta.etag {
        req = req.header("If-None-Match", etag);
    }
    let mut resp = req
        .call()
        .map_err(|e| anyhow::anyhow!("Could not download the save location database: {e}"))?;

    if resp.status().as_u16() == 304 {
        meta.checked_at = Some(Utc::now());
        fs::create_dir_all(dir)?;
        fs::write(dir.join(META_FILE), serde_json::to_string_pretty(&meta)?)?;
        return Ok(UpdateOutcome::NotModified);
    }

    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let mut body = Vec::new();
    resp.body_mut()
        .with_config()
        .limit(MAX_MANIFEST_BYTES)
        .reader()
        .read_to_end(&mut body)
        .context("Download of the save location database was interrupted")?;

    let mut db = SaveDb::from_manifest_yaml(&body)?;
    db.meta.etag = etag;
    db.meta.fetched_at = Some(Utc::now());
    db.meta.checked_at = Some(Utc::now());
    db.meta.source_url = url.to_string();
    db.save_to_dir(dir)?;
    Ok(UpdateOutcome::Updated { games: db.len() })
}

/// Converts a manifest YAML file on disk into the cached index (offline import).
pub fn import_manifest_file(yaml_path: &Path, dir: &Path) -> Result<usize> {
    let body = fs::read(yaml_path)
        .with_context(|| format!("Failed to read {}", yaml_path.display()))?;
    let mut db = SaveDb::from_manifest_yaml(&body)?;
    db.meta.fetched_at = Some(Utc::now());
    db.meta.checked_at = Some(Utc::now());
    db.meta.source_url = yaml_path.display().to_string();
    db.save_to_dir(dir)?;
    Ok(db.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
"Elden Ring":
  files:
    "<winAppData>/EldenRing/<storeUserId>":
      tags: [save]
      when:
        - os: windows
    "<winAppData>/EldenRing/GraphicsConfig.xml":
      tags: [config]
      when:
        - os: windows
  installDir:
    "ELDEN RING": {}
  steam:
    id: 1245620
"No Files Game":
  steam:
    id: 5
"Klaus":
  files:
    "<base>/save": {}
  id:
    steamExtra: [77, 78]
    lutris: klaus-game
"-KLAUS-":
  alias: Klaus
"#;

    #[test]
    fn parses_manifest_and_indexes_ids_titles_and_aliases() {
        let db = SaveDb::from_manifest_yaml(SAMPLE.as_bytes()).unwrap();
        assert_eq!(db.len(), 2, "games without file info are dropped");

        let er = db.by_steam_id(1245620).unwrap();
        assert_eq!(er.title, "Elden Ring");
        assert_eq!(er.files.len(), 2);
        assert!(er.files.iter().any(|f| f.save && f.path.ends_with("<storeUserId>")));
        assert!(er.files.iter().any(|f| f.config && !f.save));
        assert_eq!(er.files[0].when[0].os.as_deref(), Some("windows"));

        assert_eq!(db.by_title("ELDEN RING").unwrap().title, "Elden Ring");
        assert_eq!(db.by_install_dir_name("elden ring").unwrap().title, "Elden Ring");
        assert_eq!(db.by_steam_id(78).unwrap().title, "Klaus");
        assert_eq!(db.by_lutris_slug("Klaus-Game").unwrap().title, "Klaus");
        assert_eq!(db.by_title("-KLAUS-").unwrap().title, "Klaus");
        assert!(db.by_steam_id(5).is_none());
    }

    #[test]
    fn title_variants_strip_editions() {
        let v = title_key_variants("The Witcher 3: Wild Hunt Remastered");
        assert_eq!(v[0], "the_witcher_3_wild_hunt_remastered");
        assert!(v.contains(&"the_witcher_3_wild_hunt".to_string()));
        let v = title_key_variants("Dragon Age: Origins - Ultimate Edition");
        assert!(v.contains(&"dragon_age_origins".to_string()));
    }

    #[test]
    fn index_roundtrips_through_cache_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut db = SaveDb::from_manifest_yaml(SAMPLE.as_bytes()).unwrap();
        db.meta.etag = Some("\"abc\"".into());
        db.save_to_dir(tmp.path()).unwrap();
        let loaded = SaveDb::load_from_dir(tmp.path()).unwrap().unwrap();
        assert_eq!(loaded.games, db.games);
        assert_eq!(loaded.meta.etag.as_deref(), Some("\"abc\""));
        assert!(loaded.by_steam_id(1245620).is_some());
        assert!(SaveDb::load_from_dir(&tmp.path().join("nope")).unwrap().is_none());
    }
}
